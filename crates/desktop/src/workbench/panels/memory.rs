//! The **Memory** page of Mawas (MW-2, `docs/assistant.md` § Memory): what Mawas remembers
//! because you approved it, as a tab of the center (one instance).
//!
//! The page lists the entries grouped by category (Profile, Facts, Notes) with where each
//! applies (Global, or a workspace), whether a model wrote it and you have not confirmed it yet
//! (*inferred*), and where it came from; adds, edits, archives and restores them; shows the
//! history of an entry; and puts the proposals agents made on top, each with **Approve**,
//! **Edit & approve** and **Reject**. Every change is a request to the daemon: nothing shows as
//! saved before the daemon's receipt came back, and the list is read again from the daemon
//! after each answer and whenever it says the memory changed (`memory.changed`), so a hidden
//! page does no work: it hears of changes only when it is shown.

use gpui::{
    AnyElement, App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Render, SharedString,
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

use super::common::{edit_text, field_box};
use crate::sessions::MemoryChanged;
use crate::workbench::chat::vim::Vim;
use crate::workbench::chat::{ButtonKind, Look, TextInput, button};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::text_field::{FieldMut, FieldRef};
use crate::workbench::ui;
use crate::workbench::view::Region;
use crate::workbench::vim_ui::VimConfig;

/// The widest the list gets (46 rem).
const PAGE_WIDTH: f32 = 736.0;

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

struct Editor {
    target: EditorTarget,
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
            history: None,
            error: None,
            visible: false,
            stale: true,
            refreshing: false,
            refresh_again: false,
            focus: cx.focus_handle(),
            focus_wanted: false,
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

    fn open_editor(&mut self, editor: Editor, cx: &mut Context<Self>) {
        self.editor = Some(editor);
        self.error = None;
        self.focus_wanted = true;
        cx.notify();
    }

    fn new_editor(&self, target: EditorTarget, text: &str, cx: &App) -> Editor {
        let mut input = TextInput::new();
        input.reset_to(text);
        // Where a new entry applies: the workspace the page is filtered to, else global.
        let workspace = self.workspace(cx).map(|(id, _)| id);
        let scoped = matches!(self.filter, ScopeChoice::Workspace) && workspace.is_some();
        Editor {
            target,
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
        let editor = self.new_editor(EditorTarget::Entry { id, revision }, &body, cx);
        self.open_editor(editor, cx);
    }

    /// Opens "Edit & approve" on a proposal's text.
    pub fn edit_proposal(&mut self, proposal: ApprovalId, cx: &mut Context<Self>) {
        let Some(found) = self.proposals.iter().find(|p| p.id == proposal) else {
            return;
        };
        let body = found.body.clone();
        let editor = self.new_editor(EditorTarget::Proposal(proposal), &body, cx);
        self.open_editor(editor, cx);
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

    pub fn cancel_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
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

    fn transition(&mut self, entry: &str, archive: bool, cx: &mut Context<Self>) {
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

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let config = VimConfig::of(&self.env.settings);
        let Some(editor) = &mut self.editor else {
            return;
        };
        if edit_text(&mut editor.input, &mut editor.vim, config, event, true, cx) {
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let ks = &event.keystroke;
        match ks.key.as_str() {
            "enter" if ks.modifiers.platform => {
                self.save_editor(cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.cancel_editor(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
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

impl MemoryPanel {
    fn chip(
        &self,
        id: &'static str,
        label: impl Into<SharedString>,
        selected: bool,
        enabled: bool,
        look: &Look,
    ) -> gpui::Stateful<gpui::Div> {
        let (theme, tokens) = (look.theme, look.tokens);
        let label: SharedString = label.into();
        div()
            .id(SharedString::from(format!("memory-chip-{id}")))
            .relative()
            .flex_none()
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing / 2.0))
            .rounded(px(crate::theme::pill_radius()))
            .border_1()
            .border_color(if selected {
                gpui::transparent_black()
            } else {
                theme.border
            })
            .bg(if selected {
                theme.accent_soft
            } else {
                theme.surface
            })
            .text_size(px(tokens.secondary_font_size))
            .text_color(if enabled { theme.fg } else { theme.faint })
            .when(enabled, |s| s.cursor_pointer())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(label)
    }

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
        );
        let archived = self
            .chip("archived", "Show archived", self.show_archived, true, look)
            .on_click(cx.listener(|this, _, _, cx| this.toggle_archived(cx)))
            .child(probe(&self.env.regions, Region::Memory("archived")));
        div()
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 4.0))
            .py(px(tokens.spacing * 2.0))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .text_size(px(tokens.section_font_size))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child("Memory"),
                    )
                    .child(all)
                    .child(global)
                    .child(here)
                    .child(archived),
            )
            .child(
                button(
                    "memory-add",
                    "Add",
                    ButtonKind::Primary,
                    self.connected(cx),
                    look,
                )
                .on_click(cx.listener(|this, _, _, cx| this.start_add(cx)))
                .child(probe(&self.env.regions, Region::Memory("add"))),
            )
            .child(probe(&self.env.regions, Region::Memory("header")))
            .into_any_element()
    }

    fn render_proposal(
        &self,
        index: usize,
        proposal: &MemoryProposal,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let id = proposal.id;
        let action = match proposal.operation {
            MemoryOp::Create => format!(
                "remember a {}",
                category_word(proposal.category).to_lowercase()
            ),
            MemoryOp::Update => format!(
                "change a {}",
                category_word(proposal.category).to_lowercase()
            ),
            MemoryOp::Archive => "archive an entry".to_owned(),
            MemoryOp::Restore => "use an archived entry again".to_owned(),
        };
        let place = match (proposal.scope, proposal.workspace_id) {
            (MemoryScope::Workspace, Some(workspace)) => self.workspace_name(workspace, cx),
            _ => "Global".to_owned(),
        };
        let editable = matches!(proposal.operation, MemoryOp::Create | MemoryOp::Update);
        let mut card = div()
            .id(SharedString::from(format!("memory-proposal-{index}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .p(px(tokens.spacing * 3.0))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(format!(
                        "Mawas proposes to {action} · {place}"
                    ))),
            )
            .child(div().child(SharedString::from(proposal.body.clone())));
        if let Some(current) = &proposal.current_body {
            card = card.child(
                div()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(format!("Now: {current}"))),
            );
        }
        card = card.child(
            div()
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .child("Nothing is saved unless you approve."),
        );
        let approve = button(
            SharedString::from(format!("memory-approve-{index}")),
            "Approve",
            ButtonKind::Primary,
            true,
            look,
        )
        .on_click(cx.listener(move |this, _, _, cx| this.approve(id, cx)))
        .child(probe(
            &self.env.regions,
            Region::MemoryItem("approve", index),
        ));
        let edit = button(
            SharedString::from(format!("memory-edit-approve-{index}")),
            "Edit & approve",
            ButtonKind::Secondary,
            editable,
            look,
        )
        .when(editable, |b| {
            b.on_click(cx.listener(move |this, _, _, cx| this.edit_proposal(id, cx)))
        })
        .child(probe(
            &self.env.regions,
            Region::MemoryItem("edit-approve", index),
        ));
        let reject = button(
            SharedString::from(format!("memory-reject-{index}")),
            "Reject",
            ButtonKind::Secondary,
            true,
            look,
        )
        .on_click(cx.listener(move |this, _, _, cx| this.reject(id, cx)))
        .child(probe(
            &self.env.regions,
            Region::MemoryItem("reject", index),
        ));
        card.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(tokens.spacing))
                .child(approve)
                .child(edit)
                .child(reject),
        )
        .child(probe(
            &self.env.regions,
            Region::MemoryItem("proposal", index),
        ))
        .into_any_element()
    }

    fn render_editor(
        &self,
        window: &Window,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.editor.as_ref()?;
        let (theme, tokens) = (look.theme, look.tokens);
        let title = match &editor.target {
            EditorTarget::Add => "Remember something".to_owned(),
            EditorTarget::Entry { .. } => "Edit this entry".to_owned(),
            EditorTarget::Proposal(_) => "Edit the proposal, then approve it".to_owned(),
        };
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
            .key_context("MemoryEditor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .rounded(px(tokens.radius))
            .bg(theme.surface)
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(title)),
            );
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
            card = card.child(
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
                    .child(scope_chip(
                        "editor-workspace",
                        workspace
                            .as_ref()
                            .map_or_else(|| "This workspace".to_owned(), |(_, name)| name.clone()),
                        editor.scope == MemoryScope::Workspace,
                        workspace.is_some(),
                        MemoryScope::Workspace,
                        cx,
                    ))
                    .child(div().w(px(tokens.spacing * 2.0)))
                    .child(category_chip("editor-profile", MemoryCategory::Profile, cx))
                    .child(category_chip("editor-fact", MemoryCategory::Fact, cx))
                    .child(category_chip("editor-note", MemoryCategory::Note, cx)),
            );
        }
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
                        "At most {} KiB. Memory is not a secret store: keys, tokens and passwords are refused. ⌘↵ saves, Esc cancels.",
                        BODY_MAX_BYTES / 1024
                    ))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(tokens.spacing))
                    .child(
                        button(
                            "memory-save",
                            match editor.target {
                                EditorTarget::Proposal(_) => "Approve with this text",
                                _ => "Save",
                            },
                            ButtonKind::Primary,
                            true,
                            look,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.save_editor(cx)))
                        .child(probe(&self.env.regions, Region::Memory("save"))),
                    )
                    .child(
                        button("memory-cancel", "Cancel", ButtonKind::Secondary, true, look)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_editor(cx)))
                            .child(probe(&self.env.regions, Region::Memory("cancel"))),
                    ),
            )
            .child(probe(&self.env.regions, Region::Memory("editor")));
        Some(card.into_any_element())
    }

    fn render_entry(
        &self,
        index: usize,
        entry: &MemoryEntry,
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
        meta = meta.child(
            div()
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .truncate()
                .child(SharedString::from(entry.provenance.summary())),
        );
        let action = |name: &'static str,
                      label: &'static str,
                      cx: &mut Context<Self>,
                      run: fn(&mut Self, &str, &mut Context<Self>)| {
            let id = id.clone();
            button(
                SharedString::from(format!("memory-{name}-{index}")),
                label,
                ButtonKind::Ghost,
                true,
                look,
            )
            .on_click(cx.listener(move |this, _, _, cx| run(this, &id, cx)))
            .child(probe(&self.env.regions, Region::MemoryItem(name, index)))
        };
        let mut actions = div().flex().flex_wrap().gap(px(tokens.spacing / 2.0));
        if entry.inferred && !archived {
            actions = actions.child(action("confirm", "Confirm", cx, Self::confirm));
        }
        if !archived {
            actions = actions.child(action("edit", "Edit", cx, Self::start_edit));
            actions = actions.child(action("archive", "Archive", cx, Self::archive));
        } else {
            actions = actions.child(action("restore", "Restore", cx, Self::restore));
        }
        actions = actions.child(action("history", "History", cx, Self::toggle_history));
        let mut row = div()
            .id(SharedString::from(format!("memory-entry-{index}")))
            .relative()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .px(px(tokens.spacing * 3.0))
            .py(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(
                div()
                    .when(archived, |s| s.text_color(theme.muted))
                    .child(SharedString::from(entry.body.clone())),
            )
            .child(meta)
            .child(actions);
        if let Some(shown) = self.history.as_ref().filter(|h| h.entry == entry.id) {
            row = row.child(self.render_history(shown, look));
        }
        row.child(probe(&self.env.regions, Region::MemoryItem("entry", index)))
            .into_any_element()
    }

    fn render_history(&self, shown: &Shown, look: &Look) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing / 2.0))
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

    fn render_body(&self, window: &Window, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let message = |text: &str| {
            div()
                .text_color(theme.muted)
                .child(SharedString::from(text.to_owned()))
                .into_any_element()
        };
        let mut sections: Vec<AnyElement> = Vec::new();
        if !self.connected(cx) {
            sections.push(message("Not connected to the Hutan daemon."));
        } else if self.unsupported {
            sections.push(message(
                "This daemon has no memory (it is older than protocol 0.32): restart it with `hutan daemon restart`.",
            ));
        } else {
            if !self.proposals.is_empty() {
                sections.push(
                    ui::section_label(
                        format!("Waiting for you ({})", self.proposals.len()),
                        &theme,
                        &tokens,
                    )
                    .into_any_element(),
                );
                for (index, proposal) in self.proposals.iter().enumerate() {
                    sections.push(self.render_proposal(index, proposal, look, cx));
                }
            }
            if let Some(editor) = self.render_editor(window, look, cx) {
                sections.push(editor);
            }
            let mut index = 0;
            for category in MemoryCategory::ALL {
                let rows: Vec<&MemoryEntry> = self
                    .entries
                    .iter()
                    .filter(|e| e.category == category)
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                sections.push(
                    ui::section_label(category_title(category), &theme, &tokens).into_any_element(),
                );
                for entry in rows {
                    sections.push(self.render_entry(index, entry, look, cx));
                    index += 1;
                }
            }
            if self.entries.is_empty() && self.loaded {
                sections.push(message(
                    "Nothing is remembered yet. Mawas remembers what you approve: add something, or approve what it proposes.",
                ));
            }
        }
        if let Some(error) = &self.error {
            sections.insert(
                0,
                div()
                    .relative()
                    .text_color(theme.danger)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(error.clone()))
                    .child(probe(&self.env.regions, Region::Memory("error")))
                    .into_any_element(),
            );
        }
        div()
            .id("memory-body")
            .relative()
            .flex()
            .flex_1()
            .min_h_0()
            .justify_center()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .max_w(px(PAGE_WIDTH))
                    .gap(px(tokens.spacing * 2.0))
                    .p(px(tokens.spacing * 4.0))
                    .children(sections),
            )
            .into_any_element()
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
        let header = self.render_header(&look, cx);
        let body = self.render_body(window, &look, cx);
        let (theme, tokens) = (look.theme, look.tokens);
        div()
            .id("memory-panel")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.fg)
            .text_size(px(tokens.font_size))
            .child(header)
            .child(body)
    }
}
