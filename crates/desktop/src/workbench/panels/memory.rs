//! The **Memory** page of Mawas (MW-2, `docs/assistant.md` § Memory): what Mawas remembers
//! because you approved it, as a body of the left sidebar (MEM-SIDEBAR; one instance, it
//! replaces Projects until *‹ Projects*).
//!
//! One column for 240–480 px: a header with **Add**, the scope filter, the proposals agents made
//! (*Waiting for you*: what, where, **Reject** / **Edit** / **Approve**), then the entries grouped
//! by category (Profile, Facts, Notes) as one-line rows. A selected entry shows its whole text,
//! where it applies, whether a model wrote it and you have not confirmed it yet (*inferred*),
//! where it came from, and its actions (**Confirm**, **Edit**, **Archive** / **Restore**,
//! **History**); editing opens under the row, never in a modal. Keyboard: ↑↓ entries (or
//! proposals), Tab between the two, Return edits, ⌘Return saves (or approves a proposal), Esc
//! closes the editor, ⌘⌫ archives an entry (asked first) or rejects a proposal. Every change is
//! a request to the daemon: nothing shows as saved before the daemon's receipt came back, and
//! the list is read again from the daemon after each answer and whenever it says the memory
//! changed (`memory.changed`), so a hidden page does no work: it hears of changes only when it
//! is shown.

use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::*, px,
};
use hutan_client::ConnectionState;
use hutan_core::{ApprovalId, WorkspaceId};
use hutan_protocol::memory::{
    BODY_MAX_BYTES, MemoryCategory, MemoryEntry, MemoryList, MemoryOp, MemoryProposal,
    MemoryProposalResolve, MemoryRevision, MemoryScope, MemoryState, MemoryTransition,
    MemoryUpdate, MemoryWrite, ProposalDecision,
};
use hutan_protocol::messages::WorkspaceKind;

use super::common::{edit_text, field_box, panel_header_controls};
use crate::icons::Icon;
use crate::sessions::MemoryChanged;
use crate::workbench::chat::vim::Vim;
use crate::workbench::chat::{Look, TextInput};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::text_field::{FieldMut, FieldRef};
use crate::workbench::ui;
use crate::workbench::ui::{Button, StyledExt as _};
use crate::workbench::view::Region;
use crate::workbench::vim_ui::VimConfig;

/// The widest a workspace's name gets on a filter chip before it is cut with "…".
const FILTER_NAME_MAX: f32 = 120.0;

/// Which entries the page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeChoice {
    /// The global entries and those of the workspace the window shows.
    All,
    /// The global entries only.
    Global,
    /// The entries of the workspace the window shows.
    Workspace,
}

/// What the editor is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorTarget {
    /// A new entry.
    Add,
    /// A new revision of an entry, as it was read at `revision`.
    Entry { id: String, revision: u64 },
    /// "Edit & approve" of a proposal.
    Proposal(ApprovalId),
}

/// What the keyboard (and the last click) is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cursor {
    /// A proposal waiting for the user.
    Proposal(ApprovalId),
    /// An entry, by id.
    Entry(String),
}

struct Editor {
    target: EditorTarget,
    /// The text the editor opened with: what tells an edit from a look.
    original: String,
    input: TextInput,
    vim: Vim,
    /// Where a new entry applies.
    scope: MemoryScope,
    workspace: Option<WorkspaceId>,
    category: MemoryCategory,
}

/// The history an entry's row shows below it.
#[derive(Debug)]
struct Shown {
    entry: String,
    /// `None` until the daemon answered.
    revisions: Option<Vec<MemoryRevision>>,
}

pub struct MemoryPanel {
    env: PanelEnv,
    entries: Vec<MemoryEntry>,
    proposals: Vec<MemoryProposal>,
    /// The daemon answered at least once.
    loaded: bool,
    /// The daemon has no memory (older than protocol 0.32).
    unsupported: bool,
    filter: ScopeChoice,
    show_archived: bool,
    editor: Option<Editor>,
    cursor: Option<Cursor>,
    /// The entry whose archiving waits for the user's answer.
    confirm_archive: Option<String>,
    history: Option<Shown>,
    /// The failure of the last control, in the daemon's words.
    error: Option<String>,
    /// The page is on screen: a hidden page does no work.
    visible: bool,
    /// What the page shows may be out of date (something changed while it was hidden).
    stale: bool,
    refreshing: bool,
    refresh_again: bool,
    focus: FocusHandle,
    focus_wanted: bool,
    scroll: ScrollHandle,
    /// Scroll the selection into view when next drawn (the keyboard moved it).
    scroll_wanted: bool,
    /// A proposal whose editor opens once the proposals are listed ("Edit & approve" in Today).
    edit_when_loaded: Option<ApprovalId>,
    renders: usize,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for MemoryPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryPanel")
            .field("entries", &self.entries.len())
            .field("proposals", &self.proposals.len())
            .finish_non_exhaustive()
    }
}

fn operation_id() -> String {
    hutan_core::ItemId::new().to_string()
}

fn category_title(category: MemoryCategory) -> &'static str {
    match category {
        MemoryCategory::Profile => "Profile",
        MemoryCategory::Fact => "Facts",
        MemoryCategory::Note => "Notes",
    }
}

fn category_word(category: MemoryCategory) -> &'static str {
    match category {
        MemoryCategory::Profile => "Profile",
        MemoryCategory::Fact => "Fact",
        MemoryCategory::Note => "Note",
    }
}

impl MemoryPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        let observed = cx.observe(&env.sessions, |this, _, cx| this.on_sessions(cx));
        let changed = cx.subscribe(&env.sessions, |this, _, _: &MemoryChanged, cx| {
            this.on_changed(cx);
        });
        Self {
            env,
            entries: Vec::new(),
            proposals: Vec::new(),
            loaded: false,
            unsupported: false,
            filter: ScopeChoice::All,
            show_archived: false,
            editor: None,
            cursor: None,
            confirm_archive: None,
            history: None,
            error: None,
            visible: false,
            stale: true,
            refreshing: false,
            refresh_again: false,
            focus: cx.focus_handle(),
            focus_wanted: false,
            scroll: ScrollHandle::new(),
            scroll_wanted: false,
            edit_when_loaded: None,
            renders: 0,
            _subscriptions: vec![observed, changed],
        }
    }

    // ---- reading ---------------------------------------------------------------------------

    pub fn entries(&self) -> &[MemoryEntry] {
        &self.entries
    }

    /// The proposals waiting for the user, oldest first.
    pub fn proposals(&self) -> &[MemoryProposal] {
        &self.proposals
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn filter(&self) -> ScopeChoice {
        self.filter
    }

    pub fn shows_archived(&self) -> bool {
        self.show_archived
    }

    /// What the keyboard is on.
    pub fn cursor(&self) -> Option<&Cursor> {
        self.cursor.as_ref()
    }

    /// The entry the archive question is about.
    pub fn archive_question(&self) -> Option<&str> {
        self.confirm_archive.as_deref()
    }

    pub fn editor_target(&self) -> Option<&EditorTarget> {
        self.editor.as_ref().map(|editor| &editor.target)
    }

    /// The text in the editor.
    pub fn editor_text(&self) -> Option<&str> {
        self.editor.as_ref().map(|editor| editor.input.text())
    }

    /// The failure of the last control, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The revisions of the entry whose history is open, once the daemon answered.
    pub fn history_revisions(&self) -> Option<&[MemoryRevision]> {
        self.history.as_ref()?.revisions.as_deref()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// How many times the page was drawn (tests).
    pub fn render_count(&self) -> usize {
        self.renders
    }

    fn connected(&self, cx: &App) -> bool {
        self.env.sessions.read(cx).connection_state() == ConnectionState::Connected
    }

    /// The project workspace the window shows (Home has the global scope only).
    fn workspace(&self, cx: &App) -> Option<(WorkspaceId, String)> {
        self.env
            .sessions
            .read(cx)
            .active_workspace()
            .filter(|w| w.kind != WorkspaceKind::Home)
            .map(|w| (w.id, w.name.clone()))
    }

    fn workspace_name(&self, id: WorkspaceId, cx: &App) -> String {
        self.env
            .sessions
            .read(cx)
            .workspace(id)
            .map_or_else(|| "a workspace".to_owned(), |w| w.name.clone())
    }

    // ---- visibility and refresh -----------------------------------------------------------

    /// The workbench tells the page whether it is on screen. One that was hidden lists again
    /// when it is shown if anything changed meanwhile; one that is hidden is not woken.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            if self.stale {
                self.refresh(cx);
            }
            cx.notify();
        }
    }

    fn on_sessions(&mut self, cx: &mut Context<Self>) {
        // The store changes all the time; a hidden page does not look at it, and a shown one
        // only wants the connection (a page opened before the daemon was there) and the
        // workspace it shows.
        if !self.visible {
            return;
        }
        if self.stale && self.connected(cx) && !self.refreshing {
            self.refresh(cx);
        }
        cx.notify();
    }

    fn on_changed(&mut self, cx: &mut Context<Self>) {
        if self.visible {
            self.refresh(cx);
        } else {
            self.stale = true;
        }
    }

    /// The listing the page asks for now.
    fn request(&self, cx: &App) -> Option<MemoryList> {
        let workspace = self.workspace(cx).map(|(id, _)| id);
        let include_archived = self.show_archived;
        Some(match self.filter {
            ScopeChoice::All => MemoryList {
                workspace_id: workspace,
                include_archived,
                ..MemoryList::default()
            },
            ScopeChoice::Global => MemoryList {
                scope: Some(MemoryScope::Global),
                include_archived,
                ..MemoryList::default()
            },
            ScopeChoice::Workspace => MemoryList {
                scope: Some(MemoryScope::Workspace),
                workspace_id: Some(workspace?),
                include_archived,
                ..MemoryList::default()
            },
        })
    }

    /// Reads the entries and the proposals from the daemon.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.connected(cx) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        if !connection.supports_memory() {
            self.unsupported = true;
            self.stale = false;
            cx.notify();
            return;
        }
        if self.refreshing {
            self.refresh_again = true;
            return;
        }
        self.refreshing = true;
        self.stale = false;
        let request = self.request(cx);
        cx.spawn(async move |this, cx| {
            let entries = match request {
                Some(request) => connection.memory_list(request).await,
                None => Ok(Vec::new()),
            };
            let proposals = connection.memory_proposals(false).await;
            this.update(cx, |this, cx| {
                this.refreshing = false;
                match (entries, proposals) {
                    (Ok(entries), Ok(proposals)) => {
                        this.entries = entries;
                        this.proposals = proposals;
                        this.loaded = true;
                        this.unsupported = false;
                        this.settle_cursor();
                        if let Some(id) = this.edit_when_loaded.take() {
                            this.edit_proposal(id, cx);
                        }
                    }
                    (Err(error), _) | (_, Err(error)) => {
                        this.error = Some(format!("Could not read the memory: {error}"));
                    }
                }
                if std::mem::take(&mut this.refresh_again) {
                    this.refresh(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---- filters ---------------------------------------------------------------------------

    pub fn set_filter(&mut self, filter: ScopeChoice, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.refresh(cx);
        cx.notify();
    }

    pub fn toggle_archived(&mut self, cx: &mut Context<Self>) {
        self.show_archived = !self.show_archived;
        self.refresh(cx);
        cx.notify();
    }

    // ---- the editor ------------------------------------------------------------------------

    /// Whether the editor holds text the user wrote and did not save.
    fn editor_dirty(&self) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|e| e.input.text() != e.original)
    }

    /// Opens `editor` (the one that is open and has unsaved text is never dropped without a
    /// word: the user saves or cancels it first).
    fn open_editor(&mut self, editor: Editor, cx: &mut Context<Self>) {
        if self
            .editor
            .as_ref()
            .is_some_and(|open| open.target != editor.target)
            && self.editor_dirty()
        {
            self.error = Some(
                "Save or cancel what you are writing first (⌘↵ saves, Esc cancels).".to_owned(),
            );
            self.focus_wanted = true;
            cx.notify();
            return;
        }
        self.confirm_archive = None;
        self.editor = Some(editor);
        self.error = None;
        self.focus_wanted = true;
        cx.notify();
    }

    fn new_editor(&self, target: EditorTarget, text: &str, cx: &App) -> Editor {
        let mut input = TextInput::new();
        input.reset_to(text);
        let original = text.to_owned();
        // Where a new entry applies: the workspace the page is filtered to, else global.
        let workspace = self.workspace(cx).map(|(id, _)| id);
        let scoped = matches!(self.filter, ScopeChoice::Workspace) && workspace.is_some();
        Editor {
            target,
            original,
            input,
            vim: Vim::default(),
            scope: if scoped {
                MemoryScope::Workspace
            } else {
                MemoryScope::Global
            },
            workspace: scoped.then_some(workspace).flatten(),
            category: if scoped {
                MemoryCategory::Note
            } else {
                MemoryCategory::Profile
            },
        }
    }

    /// Opens the editor for a new entry.
    pub fn start_add(&mut self, cx: &mut Context<Self>) {
        let editor = self.new_editor(EditorTarget::Add, "", cx);
        self.open_editor(editor, cx);
    }

    /// Opens the editor on an entry's text.
    pub fn start_edit(&mut self, entry: &str, cx: &mut Context<Self>) {
        let Some(found) = self.entries.iter().find(|e| e.id == entry) else {
            return;
        };
        let (id, revision, body) = (found.id.clone(), found.revision, found.body.clone());
        let editor = self.new_editor(
            EditorTarget::Entry {
                id: id.clone(),
                revision,
            },
            &body,
            cx,
        );
        self.open_editor(editor, cx);
        if self
            .editor_target()
            .is_some_and(|t| matches!(t, EditorTarget::Entry { id: open, .. } if *open == id))
        {
            self.cursor = Some(Cursor::Entry(id));
        }
    }

    /// Opens "Edit & approve" on a proposal's text.
    pub fn edit_proposal(&mut self, proposal: ApprovalId, cx: &mut Context<Self>) {
        let Some(found) = self.proposals.iter().find(|p| p.id == proposal) else {
            return;
        };
        let body = found.body.clone();
        let editor = self.new_editor(EditorTarget::Proposal(proposal), &body, cx);
        self.open_editor(editor, cx);
        if self.editor_target() == Some(&EditorTarget::Proposal(proposal)) {
            self.cursor = Some(Cursor::Proposal(proposal));
        }
    }

    /// "Edit & approve" from Today, by the proposal's id as text: opens when the proposal is
    /// listed (now, or once the list arrives).
    pub fn edit_proposal_by_id(&mut self, id: &str, cx: &mut Context<Self>) {
        let Ok(id) = id.parse::<ApprovalId>() else {
            return;
        };
        if self.proposals.iter().any(|p| p.id == id) {
            self.edit_proposal(id, cx);
        } else {
            self.edit_when_loaded = Some(id);
            self.refresh(cx);
        }
    }

    /// Closes the editor without saving; the keyboard stays on the list.
    pub fn cancel_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        self.error = None;
        self.focus_wanted = true;
        cx.notify();
    }

    /// Replaces the editor's text (tests and paste-like inputs).
    pub fn set_editor_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.editor {
            editor.input.reset_to(text);
            cx.notify();
        }
    }

    /// Chooses where (and as what) a new entry applies.
    pub fn set_editor_scope(
        &mut self,
        scope: MemoryScope,
        category: Option<MemoryCategory>,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace(cx).map(|(id, _)| id);
        let Some(editor) = &mut self.editor else {
            return;
        };
        match scope {
            MemoryScope::Global => {
                editor.scope = MemoryScope::Global;
                editor.workspace = None;
            }
            MemoryScope::Workspace => {
                let Some(workspace) = workspace else {
                    return;
                };
                editor.scope = MemoryScope::Workspace;
                editor.workspace = Some(workspace);
            }
        }
        if let Some(category) = category {
            editor.category = category;
        }
        cx.notify();
    }

    fn set_editor_category(&mut self, category: MemoryCategory, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.editor {
            editor.category = category;
            cx.notify();
        }
    }

    /// Sends the editor's text to the daemon: a new entry, a new revision, or an edited
    /// approval. The editor closes when the daemon's receipt arrives.
    pub fn save_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let body = editor.input.text().trim().to_owned();
        if body.is_empty() {
            self.error = Some("Write something to remember first.".to_owned());
            cx.notify();
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let target = editor.target.clone();
        let (scope, workspace, category) = (editor.scope, editor.workspace, editor.category);
        self.error = None;
        cx.spawn(async move |this, cx| {
            let saved = match target {
                EditorTarget::Add => connection
                    .memory_write(MemoryWrite {
                        operation_id: operation_id(),
                        scope,
                        workspace_id: workspace,
                        category,
                        body,
                    })
                    .await
                    .map(|_| ()),
                EditorTarget::Entry { id, revision } => connection
                    .memory_update(MemoryUpdate {
                        operation_id: operation_id(),
                        entry_id: id,
                        expected_revision: revision,
                        body,
                    })
                    .await
                    .map(|_| ()),
                EditorTarget::Proposal(proposal) => connection
                    .memory_resolve(MemoryProposalResolve {
                        proposal_id: proposal,
                        decision: ProposalDecision::Edit,
                        edited_body: Some(body),
                    })
                    .await
                    .map(|_| ()),
            };
            this.update(cx, |this, cx| {
                match saved {
                    Ok(()) => this.editor = None,
                    Err(error) => this.error = Some(describe(&error)),
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- entries ---------------------------------------------------------------------------

    fn entry(&self, id: &str) -> Option<&MemoryEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Archives an entry (it stays in its history and in the archived view).
    pub fn archive(&mut self, entry: &str, cx: &mut Context<Self>) {
        self.transition(entry, true, cx);
    }

    /// Uses an archived entry again.
    pub fn restore(&mut self, entry: &str, cx: &mut Context<Self>) {
        self.transition(entry, false, cx);
    }

    /// Asks whether to archive an entry, under its row: Archive answers, Cancel (or Return, or
    /// Esc) does not; archiving never happens by one stray key.
    pub fn ask_archive(&mut self, entry: &str, cx: &mut Context<Self>) {
        if self
            .entry(entry)
            .is_none_or(|e| e.state == MemoryState::Archived)
        {
            return;
        }
        self.cursor = Some(Cursor::Entry(entry.to_owned()));
        self.confirm_archive = Some(entry.to_owned());
        self.focus_wanted = true;
        cx.notify();
    }

    /// Withdraws the archive question.
    pub fn cancel_archive(&mut self, cx: &mut Context<Self>) {
        if self.confirm_archive.take().is_some() {
            cx.notify();
        }
    }

    /// Closes the editor when it is the one of `entry`: its text is about to be superseded.
    fn close_editor_of(&mut self, entry: &str) {
        if matches!(
            self.editor_target(),
            Some(EditorTarget::Entry { id, .. }) if id == entry
        ) {
            self.editor = None;
        }
    }

    fn transition(&mut self, entry: &str, archive: bool, cx: &mut Context<Self>) {
        self.confirm_archive = None;
        self.close_editor_of(entry);
        let Some(found) = self.entry(entry) else {
            return;
        };
        let request = MemoryTransition {
            operation_id: operation_id(),
            entry_id: found.id.clone(),
            expected_revision: found.revision,
        };
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let done = if archive {
                connection.memory_archive(request).await
            } else {
                connection.memory_restore(request).await
            };
            this.update(cx, |this, cx| {
                if let Err(error) = done {
                    this.error = Some(describe(&error));
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Confirms an inferred entry: saves its text as it is, as the user's own revision.
    pub fn confirm(&mut self, entry: &str, cx: &mut Context<Self>) {
        self.close_editor_of(entry);
        let Some(found) = self.entry(entry) else {
            return;
        };
        let request = MemoryUpdate {
            operation_id: operation_id(),
            entry_id: found.id.clone(),
            expected_revision: found.revision,
            body: found.body.clone(),
        };
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let done = connection.memory_update(request).await;
            this.update(cx, |this, cx| {
                if let Err(error) = done {
                    this.error = Some(describe(&error));
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Shows (or hides, when it is the one shown) the history of an entry below its row.
    pub fn toggle_history(&mut self, entry: &str, cx: &mut Context<Self>) {
        if self.history.as_ref().is_some_and(|h| h.entry == entry) {
            self.history = None;
            cx.notify();
            return;
        }
        let id = entry.to_owned();
        self.history = Some(Shown {
            entry: id.clone(),
            revisions: None,
        });
        let connection = self.env.sessions.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let history = connection.memory_history(&id).await;
            this.update(cx, |this, cx| {
                match history {
                    Ok(history) => {
                        if let Some(shown) = this.history.as_mut().filter(|h| h.entry == id) {
                            shown.revisions = Some(history.revisions);
                        }
                    }
                    Err(error) => {
                        this.history = None;
                        this.error = Some(describe(&error));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    // ---- proposals -------------------------------------------------------------------------

    /// Approves a proposal as it is listed.
    pub fn approve(&mut self, proposal: ApprovalId, cx: &mut Context<Self>) {
        self.resolve(proposal, ProposalDecision::Approve, cx);
    }

    /// Rejects a proposal: nothing is saved.
    pub fn reject(&mut self, proposal: ApprovalId, cx: &mut Context<Self>) {
        self.resolve(proposal, ProposalDecision::Reject, cx);
    }

    fn resolve(
        &mut self,
        proposal: ApprovalId,
        decision: ProposalDecision,
        cx: &mut Context<Self>,
    ) {
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let done = connection
                .memory_resolve(MemoryProposalResolve {
                    proposal_id: proposal,
                    decision,
                    edited_body: None,
                })
                .await;
            this.update(cx, |this, cx| {
                match done {
                    Ok(resolved)
                        if resolved.receipt.is_none() && decision != ProposalDecision::Reject =>
                    {
                        this.error = Some(
                            "That entry changed since Mawas proposed this: nothing was saved."
                                .to_owned(),
                        );
                    }
                    Ok(_) => {}
                    Err(error) => this.error = Some(describe(&error)),
                }
                this.refresh(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- selection -------------------------------------------------------------------------

    /// The entries in the order the page draws them: Profile, Facts, Notes.
    fn rows(&self) -> Vec<&MemoryEntry> {
        MemoryCategory::ALL
            .into_iter()
            .flat_map(|category| self.entries.iter().filter(move |e| e.category == category))
            .collect()
    }

    /// Drops a cursor whose target is gone (answered, archived out of the view).
    fn settle_cursor(&mut self) {
        let gone = match &self.cursor {
            Some(Cursor::Proposal(id)) => !self.proposals.iter().any(|p| p.id == *id),
            Some(Cursor::Entry(id)) => !self.entries.iter().any(|e| e.id == *id),
            None => false,
        };
        if gone {
            self.cursor = None;
        }
        if self
            .confirm_archive
            .as_ref()
            .is_some_and(|id| !self.entries.iter().any(|e| e.id == *id))
        {
            self.confirm_archive = None;
        }
    }

    /// Puts the keyboard on the page (the nav row, ⌘K, the Today rail): the list takes keys
    /// until an editor opens.
    pub fn request_focus(&mut self, cx: &mut Context<Self>) {
        self.focus_wanted = true;
        cx.notify();
    }

    /// Selects an entry (it shows its whole text and its actions). One with unsaved edits open
    /// elsewhere stays where it is.
    pub fn select_entry(&mut self, entry: &str, cx: &mut Context<Self>) {
        if self.entry(entry).is_none() || !self.leave_editor(Some(entry), cx) {
            return;
        }
        if self.confirm_archive.as_deref() != Some(entry) {
            self.confirm_archive = None;
        }
        self.cursor = Some(Cursor::Entry(entry.to_owned()));
        self.focus_wanted = true;
        self.scroll_wanted = true;
        cx.notify();
    }

    /// Selects a proposal.
    pub fn select_proposal(&mut self, proposal: ApprovalId, cx: &mut Context<Self>) {
        if !self.proposals.iter().any(|p| p.id == proposal) || !self.leave_editor(None, cx) {
            return;
        }
        self.confirm_archive = None;
        self.cursor = Some(Cursor::Proposal(proposal));
        self.focus_wanted = true;
        self.scroll_wanted = true;
        cx.notify();
    }

    /// A click on an entry's row: selects it and opens its editor under it.
    pub fn click_entry(&mut self, entry: &str, cx: &mut Context<Self>) {
        let editing = matches!(
            self.editor_target(),
            Some(EditorTarget::Entry { id, .. }) if id == entry
        );
        let archived = self
            .entry(entry)
            .is_some_and(|e| e.state == MemoryState::Archived);
        self.select_entry(entry, cx);
        if !editing && !archived && self.cursor == Some(Cursor::Entry(entry.to_owned())) {
            self.start_edit(entry, cx);
        }
    }

    /// Closes the open editor before the selection moves, unless it holds unsaved text (the
    /// user is told to save or cancel) or is the one of `keep`. Whether the selection may move.
    fn leave_editor(&mut self, keep: Option<&str>, cx: &mut Context<Self>) -> bool {
        let Some(editor) = &self.editor else {
            return true;
        };
        let same =
            matches!(&editor.target, EditorTarget::Entry { id, .. } if Some(id.as_str()) == keep);
        if same {
            return true;
        }
        if self.editor_dirty() {
            self.error = Some(
                "Save or cancel what you are writing first (⌘↵ saves, Esc cancels).".to_owned(),
            );
            self.focus_wanted = true;
            cx.notify();
            return false;
        }
        self.editor = None;
        true
    }

    /// Moves the keyboard one step through the proposals or the entries, whichever it is on
    /// (the entries when it is on neither).
    pub fn move_cursor(&mut self, step: isize, cx: &mut Context<Self>) {
        // From nowhere, down goes to the topmost thing (the first proposal) and up to the last
        // entry.
        let on_proposals = matches!(self.cursor, Some(Cursor::Proposal(_)))
            || (self.cursor.is_none() && step > 0 && !self.proposals.is_empty());
        let count = if on_proposals {
            self.proposals.len()
        } else {
            self.rows().len()
        };
        if count == 0 {
            return;
        }
        let at = match &self.cursor {
            Some(Cursor::Proposal(id)) => self.proposals.iter().position(|p| p.id == *id),
            Some(Cursor::Entry(id)) => self.rows().iter().position(|e| e.id == *id),
            None => None,
        };
        let to = match at {
            Some(at) => (at as isize + step).clamp(0, count as isize - 1) as usize,
            None if step < 0 => count - 1,
            None => 0,
        };
        if on_proposals {
            let id = self.proposals[to].id;
            self.select_proposal(id, cx);
        } else {
            let id = self.rows()[to].id.clone();
            self.select_entry(&id, cx);
        }
    }

    /// Tab: from the proposals to the entries and back (to the first of the other group).
    pub fn switch_group(&mut self, cx: &mut Context<Self>) {
        let on_proposals = matches!(self.cursor, Some(Cursor::Proposal(_)));
        if on_proposals {
            if let Some(first) = self.rows().first().map(|e| e.id.clone()) {
                self.select_entry(&first, cx);
            }
        } else if let Some(first) = self.proposals.first().map(|p| p.id) {
            self.select_proposal(first, cx);
        } else if let Some(first) = self.rows().first().map(|e| e.id.clone())
            && self.cursor.is_none()
        {
            self.select_entry(&first, cx);
        }
    }

    // ---- keys ------------------------------------------------------------------------------

    fn field(&self, cx: &Context<Self>) -> FieldRef {
        FieldRef::new(cx.weak_entity(), self.focus.clone(), |this, cx, f| {
            let vim_on = VimConfig::of(&this.env.settings).enabled;
            if let Some(editor) = &mut this.editor {
                f(&mut FieldMut {
                    input: &mut editor.input,
                    vim: vim_on.then_some(&mut editor.vim),
                    multiline: true,
                });
                cx.notify();
            }
        })
    }

    /// Whether the key was the page's: the editor's while one is open, the list's otherwise.
    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        if self.editor.is_some() {
            return self.on_editor_key(event, cx);
        }
        let ks = &event.keystroke;
        let m = ks.modifiers;
        let plain = !m.platform && !m.control && !m.alt;
        // The archive question takes the keys that answer it; Return is not an answer (a
        // destructive question is never confirmed by the default key).
        if let Some(entry) = self.confirm_archive.clone() {
            match ks.key.as_str() {
                "backspace" if m.platform => self.archive(&entry, cx),
                "enter" | "escape" => self.cancel_archive(cx),
                _ => return false,
            }
            return true;
        }
        match ks.key.as_str() {
            "down" if plain => self.move_cursor(1, cx),
            "up" if plain => self.move_cursor(-1, cx),
            "tab" if plain => {
                let before = self.cursor.clone();
                self.switch_group(cx);
                if self.cursor == before {
                    return false;
                }
            }
            "enter" if plain => match self.cursor.clone() {
                Some(Cursor::Entry(id)) => self.start_edit(&id, cx),
                Some(Cursor::Proposal(id)) => self.edit_proposal(id, cx),
                None => return false,
            },
            "enter" if m.platform => match self.cursor {
                Some(Cursor::Proposal(id)) => self.approve(id, cx),
                _ => return false,
            },
            "backspace" if m.platform => match self.cursor.clone() {
                Some(Cursor::Entry(id)) => self.ask_archive(&id, cx),
                Some(Cursor::Proposal(id)) => self.reject(id, cx),
                None => return false,
            },
            _ => return false,
        }
        true
    }

    fn on_editor_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let config = VimConfig::of(&self.env.settings);
        let Some(editor) = &mut self.editor else {
            return false;
        };
        if edit_text(&mut editor.input, &mut editor.vim, config, event, true, cx) {
            cx.notify();
            return true;
        }
        let ks = &event.keystroke;
        match ks.key.as_str() {
            "enter" if ks.modifiers.platform => self.save_editor(cx),
            "escape" => self.cancel_editor(cx),
            _ => return false,
        }
        true
    }
}

/// The daemon's words for a failure.
fn describe(error: &hutan_client::ClientError) -> String {
    match error {
        hutan_client::ClientError::Remote(body) => body.message.clone(),
        other => other.to_string(),
    }
}

impl Focusable for MemoryPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ---- drawing -------------------------------------------------------------------------------

/// The text of the archive question.
const ARCHIVE_QUESTION: &str = "Archive this entry? It stays in its history and can be restored.";

impl MemoryPanel {
    fn chip(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        selected: bool,
        enabled: bool,
        look: &Look,
    ) -> ui::Button {
        let (theme, tokens) = (look.theme, look.tokens);
        let label: SharedString = label.into();
        ui::Button::new(
            SharedString::from(format!("memory-chip-{id}")),
            label,
            &theme,
            &tokens,
        )
        .compact()
        .ghost()
        .selected(selected)
        .disabled(!enabled)
    }

    /// The page's header (the sidebar's 32 px section header with **Add**) and the scope filter
    /// under it: *All · Global · the workspace*, *Show archived*; the row wraps when the sidebar
    /// is narrow.
    fn render_header(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let workspace = self.workspace(cx);
        let workspace_label = workspace
            .as_ref()
            .map_or_else(|| "This workspace".to_owned(), |(_, name)| name.clone());
        let choice = |this: &Self,
                      id: &'static str,
                      label: String,
                      filter: ScopeChoice,
                      enabled: bool,
                      cx: &mut Context<Self>| {
            let mut chip = this.chip(id, label, this.filter == filter, enabled, look);
            if enabled {
                chip =
                    chip.on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)));
            }
            chip.child(probe(&this.env.regions, Region::Memory(id)))
        };
        let all = choice(
            self,
            "scope-all",
            "All".to_owned(),
            ScopeChoice::All,
            true,
            cx,
        );
        let global = choice(
            self,
            "scope-global",
            "Global".to_owned(),
            ScopeChoice::Global,
            true,
            cx,
        );
        let here = choice(
            self,
            "scope-workspace",
            workspace_label,
            ScopeChoice::Workspace,
            workspace.is_some(),
            cx,
        )
        .max_width(FILTER_NAME_MAX);
        let archived = self
            .chip("archived", "Show archived", self.show_archived, true, look)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_archived(cx)))
            .child(probe(&self.env.regions, Region::Memory("archived")));
        let add = Button::new("memory-add", "Add", &theme, &tokens)
            .compact()
            .icon(Icon::ActionAdd)
            .disabled(!self.connected(cx))
            .on_click(cx.listener(|this, _, _, cx| this.start_add(cx)))
            .child(probe(&self.env.regions, Region::Memory("add")))
            .into_any_element();
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .child(panel_header_controls("Memory", vec![add], cx))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .px(px(tokens.spacing * 3.0))
                    .pb(px(tokens.spacing * 2.0))
                    .child(all)
                    .child(global)
                    .child(here)
                    .child(archived)
                    .child(probe(&self.env.regions, Region::Memory("filters"))),
            )
            .child(probe(&self.env.regions, Region::Memory("header")))
            .into_any_element()
    }

    fn selected_fill(&self, selected: bool, look: &Look) -> gpui::Hsla {
        if selected {
            look.theme.sidebar_accent
        } else {
            look.theme.surface
        }
    }

    /// A waiting proposal as a compact card: what, where, the text, and **Reject**, **Edit**,
    /// **Approve** (the primary last).
    fn render_proposal(
        &self,
        index: usize,
        proposal: &MemoryProposal,
        window: &Window,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let id = proposal.id;
        let selected = self.cursor == Some(Cursor::Proposal(id));
        let action = match proposal.operation {
            MemoryOp::Create => format!(
                "Remember a {}",
                category_word(proposal.category).to_lowercase()
            ),
            MemoryOp::Update => format!(
                "Change a {}",
                category_word(proposal.category).to_lowercase()
            ),
            MemoryOp::Archive => "Archive an entry".to_owned(),
            MemoryOp::Restore => "Use an archived entry again".to_owned(),
        };
        let place = match (proposal.scope, proposal.workspace_id) {
            (MemoryScope::Workspace, Some(workspace)) => self.workspace_name(workspace, cx),
            _ => "Global".to_owned(),
        };
        let editable = matches!(proposal.operation, MemoryOp::Create | MemoryOp::Update);
        let editing = self.editor_target() == Some(&EditorTarget::Proposal(id));
        let mut text = div()
            .id(SharedString::from(format!("memory-proposal-{index}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing / 2.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select_proposal(id, cx);
            }))
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(format!("{action} · {place}"))),
            )
            .child(
                div()
                    .when(!selected, |s| s.clamp_lines(3))
                    .child(SharedString::from(proposal.body.clone())),
            );
        if let Some(current) = &proposal.current_body {
            text = text.child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .when(!selected, |s| s.clamp_lines(2))
                    .child(SharedString::from(format!("Now: {current}"))),
            );
        }
        let approve = Button::new(
            SharedString::from(format!("memory-approve-{index}")),
            "Approve",
            &look.theme,
            &look.tokens,
        )
        .compact()
        .primary()
        .on_click(cx.listener(move |this, _, _, cx| this.approve(id, cx)))
        .child(probe(
            &self.env.regions,
            Region::MemoryItem("approve", index),
        ));
        let edit = Button::new(
            SharedString::from(format!("memory-edit-approve-{index}")),
            "Edit",
            &look.theme,
            &look.tokens,
        )
        .compact()
        .disabled(!editable)
        .when(editable, |b| {
            b.on_click(cx.listener(move |this, _, _, cx| this.edit_proposal(id, cx)))
        })
        .child(probe(
            &self.env.regions,
            Region::MemoryItem("edit-approve", index),
        ));
        let reject = Button::new(
            SharedString::from(format!("memory-reject-{index}")),
            "Reject",
            &look.theme,
            &look.tokens,
        )
        .compact()
        .on_click(cx.listener(move |this, _, _, cx| this.reject(id, cx)))
        .child(probe(
            &self.env.regions,
            Region::MemoryItem("reject", index),
        ));
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .bg(self.selected_fill(selected, look))
            .child(text)
            .when(editing, |card| {
                card.children(self.render_editor(window, look, cx))
            })
            .when(!editing, |card| {
                card.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(tokens.spacing))
                        .child(reject)
                        .child(edit)
                        .child(approve),
                )
            })
            .child(probe(
                &self.env.regions,
                Region::MemoryItem("proposal", index),
            ))
            .into_any_element()
    }

    /// The editor, under the row, card or header it belongs to: a field, where a new entry
    /// applies (while adding), what the keys do, **Cancel** and **Save**.
    fn render_editor(
        &self,
        window: &Window,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.editor.as_ref()?;
        let (theme, tokens) = (look.theme, look.tokens);
        let focused = self.focus.is_focused(window);
        let vim = VimConfig::of(&self.env.settings);
        let field = field_box(
            "memory-editor-field",
            &editor.input,
            vim.enabled.then_some(&editor.vim),
            "What should Mawas remember?",
            focused,
            true,
            Some(self.field(cx)),
            &theme,
            &tokens,
        );
        let mut card = div()
            .id("memory-editor")
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0));
        if editor.target == EditorTarget::Add {
            let workspace = self.workspace(cx);
            let scope_chip = |id: &'static str,
                              label: String,
                              selected: bool,
                              enabled: bool,
                              scope: MemoryScope,
                              cx: &mut Context<Self>| {
                let mut chip = self.chip(id, label, selected, enabled, look);
                if enabled {
                    chip = chip.on_click(cx.listener(move |this, _, _, cx| {
                        this.set_editor_scope(scope, None, cx);
                    }));
                }
                chip.child(probe(&self.env.regions, Region::Memory(id)))
            };
            let category_chip =
                |id: &'static str, category: MemoryCategory, cx: &mut Context<Self>| {
                    self.chip(
                        id,
                        category_word(category),
                        editor.category == category,
                        true,
                        look,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_editor_category(category, cx);
                    }))
                    .child(probe(&self.env.regions, Region::Memory(id)))
                };
            card = card
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(tokens.spacing))
                        .child(scope_chip(
                            "editor-global",
                            "Global".to_owned(),
                            editor.scope == MemoryScope::Global,
                            true,
                            MemoryScope::Global,
                            cx,
                        ))
                        .child(
                            scope_chip(
                                "editor-workspace",
                                workspace.as_ref().map_or_else(
                                    || "This workspace".to_owned(),
                                    |(_, name)| name.clone(),
                                ),
                                editor.scope == MemoryScope::Workspace,
                                workspace.is_some(),
                                MemoryScope::Workspace,
                                cx,
                            )
                            .max_width(FILTER_NAME_MAX),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(tokens.spacing))
                        .child(category_chip("editor-profile", MemoryCategory::Profile, cx))
                        .child(category_chip("editor-fact", MemoryCategory::Fact, cx))
                        .child(category_chip("editor-note", MemoryCategory::Note, cx)),
                );
        }
        let save_label = match editor.target {
            EditorTarget::Proposal(_) => "Approve with this text",
            _ => "Save",
        };
        card = card
            .child(
                div()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, window, cx| {
                            cx.defer_in(window, |this, window, cx| window.focus(&this.focus, cx));
                        }),
                    )
                    .child(field),
            )
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(format!(
                        "At most {} KiB; not a secret store (keys and passwords are refused). ⌘↵ saves, Esc cancels.",
                        BODY_MAX_BYTES / 1024
                    ))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_end()
                    .gap(px(tokens.spacing))
                    .child(
                        Button::new("memory-cancel", "Cancel", &look.theme, &look.tokens)
                            .compact()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_editor(cx)))
                            .child(probe(&self.env.regions, Region::Memory("cancel"))),
                    )
                    .child(
                        Button::new("memory-save", save_label, &look.theme, &look.tokens)
                            .compact()
                            .primary()
                            .on_click(cx.listener(|this, _, _, cx| this.save_editor(cx)))
                            .child(probe(&self.env.regions, Region::Memory("save"))),
                    ),
            )
            .child(probe(&self.env.regions, Region::Memory("editor")));
        Some(card.into_any_element())
    }

    /// An entry: a one-line row (the whole text in a tooltip); selected, its whole text, where
    /// it applies, where it came from and its actions; editing, the editor under it.
    fn render_entry(
        &self,
        index: usize,
        entry: &MemoryEntry,
        window: &Window,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let id = entry.id.clone();
        let archived = entry.state == MemoryState::Archived;
        let selected = self.cursor == Some(Cursor::Entry(id.clone()));
        let editing = matches!(
            self.editor_target(),
            Some(EditorTarget::Entry { id: open, .. }) if *open == id
        );
        let hover = theme.sidebar_hover;
        let body = SharedString::from(entry.body.clone());
        let click_id = id.clone();
        let mut line = div()
            .id(SharedString::from(format!("memory-entry-{index}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(tokens.spacing))
            .min_h(px(tokens.sidebar_row - 4.0))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing / 2.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .when(!selected, move |s| s.hover(move |s| s.bg(hover)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.click_entry(&click_id, cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(archived, |s| s.text_color(theme.muted))
                    .when(!selected, |s| s.truncate())
                    .child(body.clone()),
            );
        if !selected {
            if entry.inferred {
                line = line.child(ui::pill("inferred", &theme, &tokens));
            }
            let tip = body;
            line = line.tooltip(move |_, cx| {
                cx.new(|_| crate::workbench::style::TextTooltip(tip.clone()))
                    .into()
            });
        }
        let line = line.child(probe(&self.env.regions, Region::MemoryItem("entry", index)));
        let mut block = div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .rounded(px(tokens.radius))
            .when(selected, |s| s.bg(theme.sidebar_accent))
            .child(line);
        if editing {
            block = block.child(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .pb(px(tokens.spacing))
                    .children(self.render_editor(window, look, cx)),
            );
        }
        if selected {
            block = block.child(self.render_entry_details(index, entry, editing, look, cx));
        }
        if let Some(shown) = self.history.as_ref().filter(|h| h.entry == entry.id) {
            block = block.child(self.render_history(shown, look));
        }
        block
            .child(probe(&self.env.regions, Region::MemoryItem("block", index)))
            .into_any_element()
    }

    /// Under a selected entry: where it applies, where it came from, and what can be done.
    fn render_entry_details(
        &self,
        index: usize,
        entry: &MemoryEntry,
        editing: bool,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let id = entry.id.clone();
        let archived = entry.state == MemoryState::Archived;
        let place = match (entry.scope, entry.workspace_id) {
            (MemoryScope::Workspace, Some(workspace)) => self.workspace_name(workspace, cx),
            _ => "Global".to_owned(),
        };
        let mut meta = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(tokens.spacing))
            .child(ui::pill(SharedString::from(place), &theme, &tokens))
            .child(
                div()
                    .text_color(theme.faint)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(format!("r{}", entry.revision))),
            );
        if entry.inferred {
            meta = meta.child(ui::pill("inferred", &theme, &tokens).child(probe(
                &self.env.regions,
                Region::MemoryItem("inferred", index),
            )));
        }
        if archived {
            meta = meta.child(ui::pill("archived", &theme, &tokens));
        }
        let action = |name: &'static str,
                      label: &'static str,
                      cx: &mut Context<Self>,
                      run: fn(&mut Self, &str, &mut Context<Self>)| {
            let id = id.clone();
            Button::new(
                SharedString::from(format!("memory-{name}-{index}")),
                label,
                &look.theme,
                &look.tokens,
            )
            .compact()
            .ghost()
            .on_click(cx.listener(move |this, _, _, cx| run(this, &id, cx)))
            .child(probe(&self.env.regions, Region::MemoryItem(name, index)))
        };
        let mut actions = div()
            .flex()
            .flex_wrap()
            .gap(px(tokens.spacing / 2.0))
            .ml(px(-tokens.spacing));
        if entry.inferred && !archived {
            actions = actions.child(action("confirm", "Confirm", cx, Self::confirm));
        }
        if !archived {
            if !editing {
                actions = actions.child(action("edit", "Edit", cx, Self::start_edit));
            }
            actions = actions.child(action("archive", "Archive", cx, Self::ask_archive));
        } else {
            actions = actions.child(action("restore", "Restore", cx, Self::restore));
        }
        actions = actions.child(action("history", "History", cx, Self::toggle_history));
        let asking = self.confirm_archive.as_deref() == Some(entry.id.as_str());
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 2.0))
            .pb(px(tokens.spacing * 2.0))
            .child(meta)
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(entry.provenance.summary())),
            )
            .child(actions)
            .when(asking, |details| {
                details.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .child(
                            div()
                                .text_size(px(tokens.secondary_font_size))
                                .child(ARCHIVE_QUESTION),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(tokens.spacing))
                                .child(
                                    Button::new(
                                        SharedString::from(format!(
                                            "memory-archive-cancel-{index}"
                                        )),
                                        "Cancel",
                                        &look.theme,
                                        &look.tokens,
                                    )
                                    .compact()
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| this.cancel_archive(cx)))
                                    .child(probe(
                                        &self.env.regions,
                                        Region::MemoryItem("archive-cancel", index),
                                    )),
                                )
                                .child({
                                    let id = entry.id.clone();
                                    Button::new(
                                        SharedString::from(format!("memory-archive-yes-{index}")),
                                        "Archive",
                                        &look.theme,
                                        &look.tokens,
                                    )
                                    .compact()
                                    .destructive()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.archive(&id, cx);
                                    }))
                                    .child(probe(
                                        &self.env.regions,
                                        Region::MemoryItem("archive-yes", index),
                                    ))
                                }),
                        ),
                )
            })
            .child(probe(
                &self.env.regions,
                Region::MemoryItem("details", index),
            ))
            .into_any_element()
    }

    fn render_history(&self, shown: &Shown, look: &Look) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let mut list = div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing / 2.0))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing * 2.0))
            .pt(px(tokens.spacing))
            .border_t_1()
            .border_color(theme.border);
        match &shown.revisions {
            None => {
                list = list.child(div().text_color(theme.muted).child("Reading the history…"));
            }
            Some(revisions) => {
                for revision in revisions {
                    list = list.child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_color(theme.muted)
                                    .text_size(px(tokens.secondary_font_size))
                                    .child(SharedString::from(format!(
                                        "r{} · {} · {}",
                                        revision.revision,
                                        revision.operation.as_str(),
                                        revision.provenance.summary()
                                    ))),
                            )
                            .child(div().child(SharedString::from(revision.body.clone()))),
                    );
                }
            }
        }
        list.child(probe(&self.env.regions, Region::Memory("history")))
            .into_any_element()
    }

    /// The scrolling column: the failure of the last control, a new entry's editor, the
    /// proposals, then the entries by category. Returns the sections and the index of the one
    /// the keyboard is on.
    fn render_sections(
        &self,
        window: &Window,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Option<usize>) {
        let (theme, tokens) = (look.theme, look.tokens);
        let message = |text: &str| {
            div()
                .px(px(tokens.spacing))
                .text_color(theme.muted)
                .child(SharedString::from(text.to_owned()))
                .into_any_element()
        };
        let label = |text: String| {
            ui::section_label(text, &theme, &tokens)
                .px(px(tokens.spacing))
                .pt(px(tokens.spacing))
                .into_any_element()
        };
        let mut sections: Vec<AnyElement> = Vec::new();
        let mut at_cursor = None;
        if let Some(error) = &self.error {
            sections.push(
                div()
                    .relative()
                    .px(px(tokens.spacing))
                    .text_color(theme.danger)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(error.clone()))
                    .child(probe(&self.env.regions, Region::Memory("error")))
                    .into_any_element(),
            );
        }
        if !self.connected(cx) {
            sections.push(message("Not connected to the Hutan daemon."));
        } else if self.unsupported {
            sections.push(message(
                "This daemon has no memory (it is older than protocol 0.32): restart it with `hutan daemon restart`.",
            ));
        } else {
            let rows = self.rows();
            // A new entry's editor, and one whose row or card is not on the page (a filter hid
            // it), show on top.
            let placed = match self.editor_target() {
                Some(EditorTarget::Entry { id, .. }) => rows.iter().any(|e| e.id == *id),
                Some(EditorTarget::Proposal(id)) => self.proposals.iter().any(|p| p.id == *id),
                Some(EditorTarget::Add) | None => false,
            };
            if self.editor.is_some() && !placed {
                sections.extend(self.render_editor(window, look, cx));
            }
            if !self.proposals.is_empty() {
                sections.push(label(format!("Waiting for you ({})", self.proposals.len())));
                for (index, proposal) in self.proposals.iter().enumerate() {
                    if self.cursor == Some(Cursor::Proposal(proposal.id)) {
                        at_cursor = Some(sections.len());
                    }
                    sections.push(self.render_proposal(index, proposal, window, look, cx));
                }
            }
            let mut index = 0;
            for category in MemoryCategory::ALL {
                let in_category: Vec<&MemoryEntry> = rows
                    .iter()
                    .copied()
                    .filter(|e| e.category == category)
                    .collect();
                if in_category.is_empty() {
                    continue;
                }
                sections.push(label(category_title(category).to_owned()));
                for entry in in_category {
                    if self.cursor == Some(Cursor::Entry(entry.id.clone())) {
                        at_cursor = Some(sections.len());
                    }
                    sections.push(self.render_entry(index, entry, window, look, cx));
                    index += 1;
                }
            }
            if self.entries.is_empty() && self.loaded {
                sections.push(message(
                    "Nothing is remembered yet: add something, or approve what Mawas proposes.",
                ));
            }
        }
        (sections, at_cursor)
    }
}

impl Render for MemoryPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        self.env.regions.rendered(cx.entity_id());
        if self.focus_wanted {
            self.focus_wanted = false;
            window.focus(&self.focus, cx);
        }
        let look = Look::of(cx);
        let (theme, tokens) = (look.theme, look.tokens);
        let header = self.render_header(&look, cx);
        let (sections, at_cursor) = self.render_sections(window, &look, cx);
        if std::mem::take(&mut self.scroll_wanted)
            && let Some(at) = at_cursor
        {
            self.scroll.scroll_to_item(at);
        }
        let sections: Vec<AnyElement> = sections
            .into_iter()
            .map(|section| div().flex_none().child(section).into_any_element())
            .collect();
        div()
            .id("memory-panel")
            .key_context(if self.editor.is_some() {
                "MemoryEditor"
            } else {
                "MemoryPanel"
            })
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if this.on_key(event, cx) {
                    cx.stop_propagation();
                }
            }))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_color(theme.fg)
            .text_size(px(tokens.font_size))
            .child(header)
            .child(
                div()
                    .id("memory-body")
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(tokens.spacing))
                    .px(px(tokens.spacing * 2.0))
                    .pb(px(tokens.spacing * 2.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(sections),
            )
            .child(probe(&self.env.regions, Region::Memory("panel")))
    }
}
