//! **Mawas Home** (M16-B, MW-1): the Global (Home) or the active workspace's Mawas chat, as one
//! tab of the center with the **Today** rail beside it (`assistant_rail.rs`).
//!
//! The header carries the scope, the backend·model chip, the **mode chip** (Ask before acting /
//! Advisor: `assistant.mode` of the daemon, applied from the next turn), **New chat** and
//! **Stop**, the kill switch: it denies what the chat has pending and interrupts its turn.
//!
//! The panel asks the daemon for the default assistant of a scope (`assistant.open`) and shows
//! the returned session with the ordinary chat model and view (`workbench::chat`): the
//! transcript, the composer and its context chips are the chat view's. Each scope keeps its own
//! conversation: switching scope shows another session and never moves a chip, a draft or a
//! transcript from one to the other. Opening an assistant starts no terminal and no vendor
//! agent: it is a direct chat of the provider named under `[assistant]` in the daemon's
//! `daemon.toml`. Without that setting the panel says what is missing and links to Models; it
//! never picks a provider or model of its own.

use std::collections::HashMap;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::*, px,
};
use hutan_client::{ClientError, ConnectionState};
use hutan_core::agent::ModelInfo;
use hutan_core::{SessionId, WorkspaceId};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{
    AssistantBackend, AssistantConfigure, AssistantOpen, AssistantScope,
};
use hutan_protocol::messages::{Empty, Request};
use hutan_protocol::messages::{WorkspaceInfo, WorkspaceKind};
use hutan_protocol::provider::{ProviderInfo, ProviderList, ProviderModels, ProviderRef};
use serde_json::json;

use super::common::state_dot;
use crate::icons::{Icon, icon};
use crate::workbench::actions::{
    ManageProviders, ShowAssistantModeMenu, ShowAssistantModelMenu, ShowAssistantScopeMenu,
};
use crate::workbench::agents::Models;
use crate::workbench::chat::{ButtonKind, ChatModel, ChatView, ContextChip, Look, button};
use crate::workbench::harnesses::{self, Detection, HARNESSES, Listing};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::TextTooltip;
use crate::workbench::ui::{self, Mascot, StyledExt as _};
use crate::workbench::view::Region;

/// A conversation the panel keeps: one per scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKey {
    /// The Home workspace's assistant.
    Global,
    /// The assistant of one project workspace.
    Workspace(WorkspaceId),
}

impl ScopeKey {
    /// The scope the workspace's assistant has.
    pub fn of(workspace: &WorkspaceInfo) -> Self {
        if workspace.kind == WorkspaceKind::Home {
            Self::Global
        } else {
            Self::Workspace(workspace.id)
        }
    }
}

/// Which scope the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The Home assistant.
    Global,
    /// The assistant of the active workspace (the Home one while Home is active).
    Active,
    /// A workspace other than the active one, opened from a history hit.
    Pinned(WorkspaceId),
}

/// Why a scope has no assistant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The daemon has no `[assistant] provider` (the daemon's own words).
    Unconfigured(String),
    /// The daemon does not speak protocol 0.12.
    Unsupported,
    /// Anything else the daemon or the connection answered.
    Other(String),
}

/// Where opening a scope's assistant stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    Idle,
    Opening,
    Ready(SessionId),
    Failed(Failure),
}

/// One row of the scope dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeEntry {
    pub label: String,
    pub selected: bool,
    pub enabled: bool,
    pub choose: Choice,
}

/// What an assistant can run on: a harness, or a provider (`assistant.configure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendChoice {
    Harness(AssistantBackend),
    Provider(String),
}

impl BackendChoice {
    fn label(&self) -> String {
        match self {
            Self::Harness(backend) => harnesses::harness_name(backend.as_str()),
            Self::Provider(id) => format!("Provider · {id}"),
        }
    }
}

/// The backend and model chooser of the panel header.
#[derive(Debug, Clone)]
pub struct Switcher {
    /// The backend whose models are listed.
    pub backend: Option<BackendChoice>,
    pub models: Models,
}

/// What Mawas may do (`assistant.mode` of the daemon).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MawasMode {
    /// Reads freely; every change asks first, grants permitting.
    Ask,
    /// Reads and advises; changes nothing.
    Advisor,
}

impl MawasMode {
    pub const ALL: [Self; 2] = [Self::Ask, Self::Advisor];

    /// The value of `assistant.mode`.
    pub fn setting(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Advisor => "advisor",
        }
    }

    pub fn from_setting(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.setting() == value)
    }

    /// The chip's words.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask before acting",
            Self::Advisor => "Advisor",
        }
    }

    /// What the mode means, for the menu and the tooltip.
    pub fn help(self) -> &'static str {
        match self {
            Self::Ask => {
                "Mawas can act. Every change asks you first; your grants answer for what you allowed."
            }
            Self::Advisor => "Mawas reads and advises. It cannot change anything.",
        }
    }

    /// The note under the header after a switch: it applies from the next message.
    pub fn next_turn_note(self) -> String {
        format!(
            "{} applies from your next message: Mawas restarts its agent for it.",
            self.label()
        )
    }
}

/// The widest the conversation gets (46 rem).
const CONVERSATION_WIDTH: f32 = 736.0;

struct Chat {
    model: Entity<ChatModel>,
    view: Entity<ChatView>,
}

pub struct AssistantPanel {
    pub(super) env: PanelEnv,
    choice: Choice,
    scopes: HashMap<ScopeKey, Opened>,
    chats: HashMap<SessionId, Chat>,
    /// Context to attach once a scope's assistant is open ("Send to assistant" of a block).
    waiting: Vec<(ScopeKey, ContextChip)>,
    /// Put the keyboard in the composer as soon as it is drawn.
    focus_wanted: bool,
    /// The backend chooser, while it is open.
    switcher: Option<Switcher>,
    providers: Vec<ProviderInfo>,
    was_connected: bool,
    /// `assistant.mode` as the daemon says; `None` until it has been read.
    mode: Option<MawasMode>,
    /// The daemon's settings were asked for (once per connection).
    policy_asked: bool,
    /// Under the header: a mode switch applies from the next message.
    mode_note: Option<String>,
    /// A failure of the controls of this page (a refused mode switch, an answer the daemon
    /// refused).
    error: Option<String>,
    /// The Today rail is showing.
    pub(super) rail_open: bool,
    /// The page is on screen (the active tab of a pane). A hidden page does no work: the store
    /// changes no longer wake it.
    visible: bool,
    /// How many times the page was drawn (tests).
    renders: usize,
    _subscription: Subscription,
}

impl std::fmt::Debug for AssistantPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssistantPanel")
            .field("choice", &self.choice)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl AssistantPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&env.sessions, |this, _, cx| this.on_sessions(cx));
        let connected = env.sessions.read(cx).connection_state() == ConnectionState::Connected;
        Self {
            env,
            choice: Choice::Active,
            scopes: HashMap::new(),
            chats: HashMap::new(),
            waiting: Vec::new(),
            focus_wanted: false,
            switcher: None,
            providers: Vec::new(),
            was_connected: connected,
            mode: None,
            policy_asked: false,
            mode_note: None,
            error: None,
            rail_open: true,
            visible: false,
            renders: 0,
            _subscription: subscription,
        }
    }

    // ---- state ---------------------------------------------------------------------------

    /// The active workspace when it is a project (the Home workspace has the global scope).
    fn active_project(&self, cx: &App) -> Option<WorkspaceInfo> {
        self.env
            .sessions
            .read(cx)
            .active_workspace()
            .filter(|w| w.kind != WorkspaceKind::Home)
            .cloned()
    }

    /// The scope the panel shows now.
    pub fn key(&self, cx: &App) -> ScopeKey {
        match self.choice {
            Choice::Global => ScopeKey::Global,
            Choice::Pinned(id) => ScopeKey::Workspace(id),
            Choice::Active => self
                .active_project(cx)
                .map_or(ScopeKey::Global, |w| ScopeKey::Workspace(w.id)),
        }
    }

    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// How opening `key`'s assistant stands.
    pub fn state(&self, key: ScopeKey) -> Opened {
        self.scopes.get(&key).cloned().unwrap_or(Opened::Idle)
    }

    /// The session of the scope the panel shows, once it is open.
    pub fn session(&self, cx: &App) -> Option<SessionId> {
        match self.state(self.key(cx)) {
            Opened::Ready(session) => Some(session),
            _ => None,
        }
    }

    /// The session `key`'s scope has, whether or not it is the one shown.
    pub fn session_of(&self, key: ScopeKey) -> Option<SessionId> {
        match self.state(key) {
            Opened::Ready(session) => Some(session),
            _ => None,
        }
    }

    /// The chat view of `session`, once the panel has opened it.
    pub fn chat_view(&self, session: SessionId) -> Option<Entity<ChatView>> {
        self.chats.get(&session).map(|chat| chat.view.clone())
    }

    /// The chat model of `session`, once the panel has opened it.
    pub fn chat_model(&self, session: SessionId) -> Option<Entity<ChatModel>> {
        self.chats.get(&session).map(|chat| chat.model.clone())
    }

    fn connected(&self, cx: &App) -> bool {
        self.env.sessions.read(cx).connection_state() == ConnectionState::Connected
    }

    /// Reads what the store says: a reconnect tries again what failed, a daemon that forgot a
    /// session (another epoch without it) opens that scope again.
    fn on_sessions(&mut self, cx: &mut Context<Self>) {
        let (connected, synced) = {
            let store = self.env.sessions.read(cx);
            (
                store.connection_state() == ConnectionState::Connected,
                store.synced(),
            )
        };
        if connected && !self.was_connected {
            self.scopes
                .retain(|_, state| matches!(state, Opened::Ready(_)));
        }
        if synced {
            let store = self.env.sessions.read(cx);
            self.scopes.retain(|_, state| match state {
                Opened::Ready(session) => store.session(*session).is_some(),
                _ => true,
            });
        }
        if !connected {
            self.policy_asked = false;
        }
        self.was_connected = connected;
        // A hidden page does no work: it draws again when it is shown.
        if self.visible {
            cx.notify();
        }
    }

    // ---- visibility ------------------------------------------------------------------------

    /// The workbench tells the page whether it is on screen. A page that was hidden draws again
    /// when it is shown; one that is hidden is not woken by changes of the store.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            // What the daemon says about the mode may have changed (Settings) since it was shown.
            self.policy_asked = false;
            cx.notify();
        }
    }

    /// Whether the page is on screen.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// How many times the page was drawn.
    pub fn render_count(&self) -> usize {
        self.renders
    }

    // ---- mode, new chat, stop --------------------------------------------------------------

    /// What Mawas may do, as the daemon says; `None` until it answered.
    pub fn mode(&self) -> Option<MawasMode> {
        self.mode
    }

    /// The note under the header ("Advisor applies from your next message …").
    pub fn mode_note(&self) -> Option<&str> {
        self.mode_note.as_deref()
    }

    /// The failure of the last control, if any.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(super) fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    /// Reads `assistant.mode` from the daemon (once per connection).
    fn ensure_policy(&mut self, cx: &mut Context<Self>) {
        if self.policy_asked || !self.connected(cx) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        if !connection.supports_daemon_settings() {
            // An older daemon has one mode: the read-only one it always had.
            self.policy_asked = true;
            self.mode = Some(MawasMode::Advisor);
            return;
        }
        self.policy_asked = true;
        cx.spawn(async move |this, cx| {
            let settings = connection.daemon_settings().await;
            this.update(cx, |this, cx| {
                match settings {
                    Ok(settings) => {
                        this.mode = settings
                            .entries
                            .iter()
                            .find(|e| e.key == "assistant.mode")
                            .and_then(|e| e.value.as_str())
                            .and_then(MawasMode::from_setting);
                    }
                    Err(_) => this.policy_asked = false,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Switches Mawas to `mode` (`assistant.mode`): the daemon applies it from the next turn,
    /// restarting the harness session as it needs, and the page says so.
    pub fn set_mode(&mut self, mode: MawasMode, cx: &mut Context<Self>) {
        if self.mode == Some(mode) || !self.connected(cx) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let stored = connection
                .daemon_setting_set("assistant.mode", Some(json!(mode.setting())))
                .await;
            this.update(cx, |this, cx| {
                match stored {
                    Ok(_) => {
                        this.mode = Some(mode);
                        this.mode_note = Some(mode.next_turn_note());
                    }
                    Err(error) => this.error = Some(format!("Could not switch the mode: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Starts a new conversation in the shown scope and shows it.
    pub fn new_chat(&mut self, cx: &mut Context<Self>) {
        if !self.connected(cx) {
            return;
        }
        let key = self.key(cx);
        let connection = self.env.sessions.read(cx).connection().clone();
        let request = match key {
            ScopeKey::Global => AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: true,
            },
            ScopeKey::Workspace(id) => AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(id),
                new: true,
            },
        };
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = connection.assistant_open(request).await;
            this.update(cx, |this, cx| {
                this.opened(key, result, cx);
                this.focus_composer(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Whether Stop has something to stop: a turn runs, or a request of the shown chat waits.
    pub fn can_stop(&self, cx: &App) -> bool {
        let Some(session) = self.session(cx) else {
            return false;
        };
        let store = self.env.sessions.read(cx);
        store
            .session(session)
            .and_then(|info| info.chat.as_ref())
            .is_some_and(|chat| chat.turn_active || chat.pending_approvals > 0)
            || store.approvals().iter().any(|a| a.session_id == session)
    }

    /// **Stop Mawas**, the kill switch: denies every request the shown chat has pending (its
    /// own Hutan tools and its harness's native ones), then interrupts its turn.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session(cx) else {
            return;
        };
        let denials: Vec<(hutan_core::ApprovalId, String)> = self
            .env
            .sessions
            .read(cx)
            .approvals()
            .iter()
            .filter(|a| a.session_id == session)
            .filter_map(|a| {
                let refuse = a.options.iter().find(|o| {
                    matches!(
                        o.kind,
                        hutan_core::agent::PermissionOptionKind::RejectOnce
                            | hutan_core::agent::PermissionOptionKind::RejectAlways
                    )
                })?;
                Some((a.id, refuse.id.clone()))
            })
            .collect();
        let tasks: Vec<_> = denials
            .into_iter()
            .map(|(approval, option)| {
                self.env
                    .sessions
                    .update(cx, |store, cx| store.respond_approval(approval, option, cx))
            })
            .collect();
        let connection = self.env.sessions.read(cx).connection().clone();
        self.error = None;
        cx.spawn(async move |this, cx| {
            let mut failed = None;
            for task in tasks {
                if let Err(error) = task.await {
                    failed = Some(error);
                }
            }
            // The turn may have ended with its last request: nothing to interrupt then.
            let interrupted = connection.chat_interrupt(session).await;
            this.update(cx, |this, cx| {
                if let Some(error) = failed {
                    this.error = Some(format!("Could not deny a request: {error}"));
                } else if let Err(error) = interrupted
                    && !matches!(&error, ClientError::Remote(body) if body.code == ErrorCode::InvalidState)
                {
                    this.error = Some(format!("Could not stop Mawas: {error}"));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- opening -------------------------------------------------------------------------

    /// Asks the daemon for the shown scope's assistant when that has not been asked yet.
    pub fn ensure_open(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        if self.state(key) == Opened::Idle {
            self.open(key, cx);
        }
    }

    fn open(&mut self, key: ScopeKey, cx: &mut Context<Self>) {
        let (connection, synced) = {
            let store = self.env.sessions.read(cx);
            (store.connection().clone(), store.synced())
        };
        if !self.connected(cx) || !synced {
            return;
        }
        if !connection.supports_assistant() {
            self.scopes
                .insert(key, Opened::Failed(Failure::Unsupported));
            return;
        }
        self.scopes.insert(key, Opened::Opening);
        let request = match key {
            ScopeKey::Global => AssistantOpen {
                scope: AssistantScope::Global,
                workspace_id: None,
                new: false,
            },
            ScopeKey::Workspace(id) => AssistantOpen {
                scope: AssistantScope::Workspace,
                workspace_id: Some(id),
                new: false,
            },
        };
        cx.spawn(async move |this, cx| {
            let result = connection.assistant_open(request).await;
            this.update(cx, |this, cx| this.opened(key, result, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn opened(
        &mut self,
        key: ScopeKey,
        result: Result<hutan_protocol::messages::SessionInfo, ClientError>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(info) => {
                self.ensure_chat(info.id, cx);
                self.scopes.insert(key, Opened::Ready(info.id));
                self.flush(key, info.id, cx);
            }
            Err(ClientError::Remote(body)) if body.code == ErrorCode::InvalidState => {
                self.scopes
                    .insert(key, Opened::Failed(Failure::Unconfigured(body.message)));
                self.waiting.retain(|(waiting, _)| *waiting != key);
            }
            Err(error) => {
                self.scopes
                    .insert(key, Opened::Failed(Failure::Other(error.to_string())));
                self.waiting.retain(|(waiting, _)| *waiting != key);
            }
        }
        cx.notify();
    }

    fn ensure_chat(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.chats.contains_key(&session) {
            return;
        }
        let connection = self.env.sessions.read(cx).connection().clone();
        let model = cx.new(|cx| ChatModel::new(session, connection, cx));
        let (sessions, settings, regions) = (
            self.env.sessions.clone(),
            self.env.settings.clone(),
            self.env.regions.clone(),
        );
        let shown = model.clone();
        let view = cx.new(|cx| ChatView::new(session, shown, sessions, settings, regions, cx));
        self.chats.insert(session, Chat { model, view });
    }

    fn flush(&mut self, key: ScopeKey, session: SessionId, cx: &mut Context<Self>) {
        let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|(waiting, _)| *waiting == key);
        self.waiting = rest;
        if let Some(chat) = self.chats.get(&session) {
            for (_, chip) in ready {
                chat.model
                    .update(cx, |model, cx| model.attach_context(chip, cx));
            }
        }
    }

    /// Shows `choice`'s assistant (opening it on first use).
    pub fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.choice = choice;
        self.ensure_open(cx);
        cx.notify();
    }

    /// Tries again what failed for the shown scope.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        if matches!(self.state(key), Opened::Failed(_)) {
            self.scopes.remove(&key);
        }
        self.ensure_open(cx);
        cx.notify();
    }

    /// Shows the assistant `session` (a hit of the history index, say) in its own scope. `false`
    /// when it is not an assistant session this window knows.
    pub fn show_session(&mut self, session: SessionId, cx: &mut Context<Self>) -> bool {
        let found = {
            let store = self.env.sessions.read(cx);
            store
                .session(session)
                .filter(|info| info.chat.as_ref().is_some_and(|chat| chat.is_assistant()))
                .and_then(|info| store.workspace(info.workspace_id))
                .map(|workspace| {
                    let key = ScopeKey::of(workspace);
                    let choice = match key {
                        ScopeKey::Global => Choice::Global,
                        ScopeKey::Workspace(id) if store.active_workspace_id() == Some(id) => {
                            Choice::Active
                        }
                        ScopeKey::Workspace(id) => Choice::Pinned(id),
                    };
                    (key, choice)
                })
        };
        let Some((key, choice)) = found else {
            return false;
        };
        self.ensure_chat(session, cx);
        self.scopes.insert(key, Opened::Ready(session));
        self.choice = choice;
        cx.notify();
        true
    }

    /// Attaches `chip` to the composer of the assistant `session` and shows it.
    pub fn attach_to_session(
        &mut self,
        session: SessionId,
        chip: ContextChip,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.show_session(session, cx) {
            return false;
        }
        match self.chats.get(&session) {
            Some(chat) => {
                chat.model
                    .update(cx, |model, cx| model.attach_context(chip, cx));
                true
            }
            None => false,
        }
    }

    /// Attaches `chip` to the composer of `key`'s assistant and shows it, opening the assistant
    /// first when need be. The chip waits for the answer of `assistant.open`.
    pub fn attach_to_scope(&mut self, key: ScopeKey, chip: ContextChip, cx: &mut Context<Self>) {
        self.choice = match key {
            ScopeKey::Global => Choice::Global,
            ScopeKey::Workspace(id)
                if self.env.sessions.read(cx).active_workspace_id() == Some(id) =>
            {
                Choice::Active
            }
            ScopeKey::Workspace(id) => Choice::Pinned(id),
        };
        match self.state(key) {
            Opened::Ready(session) => {
                if let Some(chat) = self.chats.get(&session) {
                    chat.model
                        .update(cx, |model, cx| model.attach_context(chip, cx));
                }
            }
            _ => {
                self.waiting.push((key, chip));
                if matches!(self.state(key), Opened::Failed(_)) {
                    self.scopes.remove(&key);
                }
                self.open(key, cx);
            }
        }
        cx.notify();
    }

    // ---- backend ---------------------------------------------------------------------------

    /// The chooser of the backend and model of the shown scope's assistant.
    pub fn switcher(&self) -> Option<&Switcher> {
        self.switcher.as_ref()
    }

    /// Opens or closes the chooser (the header's backend button).
    pub fn toggle_switcher(&mut self, cx: &mut Context<Self>) {
        if self.switcher.take().is_some() {
            cx.notify();
            return;
        }
        self.switcher = Some(Switcher {
            backend: None,
            models: Models::Loading,
        });
        self.env
            .harnesses
            .update(cx, |directory, cx| directory.refresh(cx));
        let connection = self.env.sessions.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let list = connection
                .call::<ProviderList>(Request::ProviderList(Empty {}))
                .await;
            this.update(cx, |this, cx| {
                if let Ok(list) = list {
                    this.providers = list.providers;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Lists the models of `backend` in the chooser.
    pub fn choose_backend(&mut self, backend: BackendChoice, cx: &mut Context<Self>) {
        let Some(switcher) = &mut self.switcher else {
            return;
        };
        switcher.backend = Some(backend.clone());
        switcher.models = Models::Loading;
        match backend {
            BackendChoice::Harness(harness) => {
                let id = harness.as_str();
                self.env
                    .harnesses
                    .update(cx, |directory, cx| directory.load_models(id, cx));
                // The directory answers; the chooser shows what it holds when it is drawn.
            }
            BackendChoice::Provider(provider) => {
                let connection = self.env.sessions.read(cx).connection().clone();
                cx.spawn(async move |this, cx| {
                    let answer = connection
                        .call::<ProviderModels>(Request::ProviderModels(ProviderRef {
                            provider_id: provider.clone(),
                        }))
                        .await
                        .map(|m| m.models)
                        .map_err(|e| e.to_string());
                    this.update(cx, |this, cx| {
                        if let Some(switcher) = &mut this.switcher
                            && switcher.backend == Some(BackendChoice::Provider(provider))
                        {
                            switcher.models = match answer {
                                Ok(models) => Models::Ready(models),
                                Err(error) => Models::Failed(error),
                            };
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// The models the chooser lists for its backend.
    fn switcher_models(&self, cx: &App) -> Models {
        let Some(switcher) = &self.switcher else {
            return Models::Loading;
        };
        match &switcher.backend {
            Some(BackendChoice::Harness(harness)) => self
                .env
                .harnesses
                .read(cx)
                .models(harness.as_str())
                .cloned()
                .unwrap_or(Models::Loading),
            _ => switcher.models.clone(),
        }
    }

    /// Makes `backend` (and `model`) what the shown scope's assistants run on: stores it
    /// (`assistant.configure`) and opens the scope's assistant again.
    pub fn use_backend(
        &mut self,
        backend: BackendChoice,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let key = self.key(cx);
        let connection = self.env.sessions.read(cx).connection().clone();
        let (kind, provider) = match backend {
            BackendChoice::Harness(kind) => (kind, None),
            BackendChoice::Provider(id) => (AssistantBackend::Provider, Some(id)),
        };
        let (scope, workspace_id) = match key {
            ScopeKey::Global => (AssistantScope::Global, None),
            ScopeKey::Workspace(id) => (AssistantScope::Workspace, Some(id)),
        };
        self.switcher = None;
        self.scopes.insert(key, Opened::Opening);
        cx.spawn(async move |this, cx| {
            let stored = connection
                .assistant_configure(AssistantConfigure {
                    scope,
                    workspace_id,
                    backend: kind,
                    model,
                    provider,
                })
                .await;
            this.update(cx, |this, cx| match stored {
                Ok(_) => {
                    let pinned = match key {
                        ScopeKey::Global => None,
                        ScopeKey::Workspace(id) => Some(id),
                    };
                    this.env.sessions.update(cx, |store, cx| {
                        store.note_assistant_pinned(pinned, true, cx)
                    });
                    this.scopes.remove(&key);
                    this.open(key, cx);
                }
                Err(error) => this.opened(key, Err(error), cx),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The first-run card's "Use OMP": OMP for every assistant that has no choice of its own.
    pub fn use_omp_everywhere(&mut self, cx: &mut Context<Self>) {
        let key = self.key(cx);
        let connection = self.env.sessions.read(cx).connection().clone();
        self.scopes.insert(key, Opened::Opening);
        cx.spawn(async move |this, cx| {
            let stored = connection
                .assistant_configure(AssistantConfigure {
                    scope: AssistantScope::Global,
                    workspace_id: None,
                    backend: AssistantBackend::Omp,
                    model: None,
                    provider: None,
                })
                .await;
            this.update(cx, |this, cx| match stored {
                Ok(_) => {
                    this.env
                        .sessions
                        .update(cx, |store, cx| store.note_assistant_pinned(None, true, cx));
                    this.scopes.remove(&key);
                    this.open(key, cx);
                }
                Err(error) => this.opened(key, Err(error), cx),
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Puts the keyboard in the composer of the shown assistant (as soon as it is drawn).
    pub fn focus_composer(&mut self, cx: &mut Context<Self>) {
        self.focus_wanted = true;
        cx.notify();
    }

    // ---- rendering -----------------------------------------------------------------------

    /// The rows of the scope dropdown: Global, and the workspace the panel follows (greyed out
    /// while there is none).
    pub fn scope_entries(&self, cx: &App) -> Vec<ScopeEntry> {
        let key = self.key(cx);
        let project = self.active_project(cx);
        let pinned = matches!(self.choice, Choice::Pinned(_));
        let workspace_label = match (&self.choice, &project) {
            (Choice::Pinned(id), _) => self.env.sessions.read(cx).workspace(*id).map_or_else(
                || "Workspace".to_owned(),
                |w| format!("Workspace · {}", w.name),
            ),
            (_, Some(project)) => format!("Workspace · {}", project.name),
            _ => "Workspace".to_owned(),
        };
        vec![
            ScopeEntry {
                label: "Global".to_owned(),
                selected: !matches!(key, ScopeKey::Workspace(_)),
                enabled: true,
                choose: Choice::Global,
            },
            ScopeEntry {
                label: workspace_label,
                selected: matches!(key, ScopeKey::Workspace(_)),
                enabled: project.is_some() || pinned,
                choose: if pinned { self.choice } else { Choice::Active },
            },
        ]
    }

    /// The session whose model the menu's Switch model… changes: the shown assistant when it
    /// runs on a harness or a provider and is between turns.
    pub fn switchable_session(&self, cx: &App) -> Option<SessionId> {
        if !self.connected(cx) {
            return None;
        }
        let session = self.session(cx)?;
        let chat = self.env.sessions.read(cx).session(session)?.chat.as_ref()?;
        (!chat.turn_active).then_some(session)
    }

    fn render_header(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let key = self.key(cx);
        let scope_label = match key {
            ScopeKey::Global => "Global".to_owned(),
            ScopeKey::Workspace(id) => self
                .env
                .sessions
                .read(cx)
                .workspace(id)
                .map_or_else(|| "Workspace".to_owned(), |w| w.name.clone()),
        };
        let scope_note = match key {
            ScopeKey::Global => "Sees the Home folder only".to_owned(),
            ScopeKey::Workspace(id) => {
                let store = self.env.sessions.read(cx);
                store.workspace(id).map_or_else(String::new, |w| {
                    let roots: Vec<String> =
                        w.roots.iter().map(|r| r.display().to_string()).collect();
                    format!("Sees {}", roots.join(", "))
                })
            }
        };
        let state = {
            let store = self.env.sessions.read(cx);
            self.session(cx)
                .and_then(|session| store.session(session))
                .map(|info| store.presentation(info))
        };
        let hover = theme.surface_hover;
        let scope = div()
            .id("assistant-scope")
            .relative()
            .flex()
            .flex_none()
            .min_w_0()
            .max_w(px(112.0))
            .items_center()
            .gap(px(4.0))
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(tokens.radius))
            .cursor_pointer()
            .text_size(px(tokens.secondary_font_size))
            .hover(move |s| s.bg(hover))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(scope_note.clone().into())).into())
            .on_click(cx.listener(|_, e: &gpui::ClickEvent, window, cx| {
                window.dispatch_action(Box::new(ShowAssistantScopeMenu(e.position())), cx);
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(scope_label)),
            )
            .child(icon(Icon::ChevronDown, 12.0, theme.muted))
            .child(probe(&self.env.regions, Region::Assistant("scope")));
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .h(px(tokens.pane_header))
            .px(px(tokens.spacing * 2.0))
            .border_b_1()
            .border_color(theme.border)
            .child(scope)
            .children(state.map(|presentation| {
                let label = SharedString::from(presentation.label.clone());
                div()
                    .id("assistant-state")
                    .flex_none()
                    .tooltip(move |_, cx| cx.new(|_| TextTooltip(label.clone())).into())
                    .child(state_dot(theme, &presentation))
            }))
            .child(self.backend_chip(look, cx))
            .child(self.mode_chip(look, cx))
            .child(self.header_button(
                "assistant-new",
                "New chat",
                "new",
                self.connected(cx),
                look,
                cx.listener(|this, _, _, cx| this.new_chat(cx)),
            ))
            .child(self.stop_button(look, cx))
            .child(self.header_button(
                "assistant-rail",
                if self.rail_open {
                    "Hide Today"
                } else {
                    "Today"
                },
                "rail",
                true,
                look,
                cx.listener(|this, _, _, cx| this.toggle_rail(cx)),
            ))
            .child(probe(&self.env.regions, Region::Assistant("header")))
            .into_any_element()
    }

    /// A small text button of the header.
    fn header_button(
        &self,
        id: &'static str,
        label: &'static str,
        region: &'static str,
        enabled: bool,
        look: &Look,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let hover = theme.surface_hover;
        div()
            .id(id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .text_size(px(tokens.secondary_font_size))
            .when(enabled, move |s| {
                s.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(on_click)
            })
            .when(!enabled, |s| s.opacity(0.5))
            .child(label)
            .child(probe(&self.env.regions, Region::Assistant(region)))
            .into_any_element()
    }

    /// **Stop**: the kill switch. It looks like a danger button while there is something to stop.
    fn stop_button(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let enabled = self.can_stop(cx);
        let danger = theme.danger;
        div()
            .id("assistant-stop")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(if enabled { danger } else { theme.border })
            .text_color(if enabled { danger } else { theme.muted })
            .text_size(px(tokens.secondary_font_size))
            .when(enabled, move |s| {
                s.cursor_pointer()
                    .hover(move |s| s.bg(danger.opacity(0.12)))
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
            })
            .when(!enabled, |s| s.opacity(0.5))
            .tooltip(|_, cx| {
                cx.new(|_| {
                    TextTooltip("Stop Mawas: deny what it asks and interrupt its turn".into())
                })
                .into()
            })
            .child("Stop")
            .child(probe(&self.env.regions, Region::Assistant("stop")))
            .into_any_element()
    }

    /// The mode chip: what Mawas may do, opening the menu that switches it.
    fn mode_chip(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let label = self.mode.map_or("Mode…", MawasMode::label);
        let help = self.mode.map_or("", MawasMode::help);
        let enabled = self.connected(cx) && self.mode.is_some();
        let hover = theme.surface_hover;
        div()
            .id("assistant-mode")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .text_size(px(tokens.secondary_font_size))
            .when(enabled, move |s| {
                s.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|_, e: &gpui::ClickEvent, window, cx| {
                        window.dispatch_action(Box::new(ShowAssistantModeMenu(e.position())), cx);
                    }))
            })
            .when(!enabled, |s| s.opacity(0.5))
            .tooltip(move |_, cx| cx.new(|_| TextTooltip(help.into())).into())
            .child(label)
            .child(icon(Icon::ChevronDown, 12.0, theme.muted))
            .child(probe(&self.env.regions, Region::Assistant("mode")))
            .into_any_element()
    }

    /// The model the shown assistant's session runs, when it runs on `choice`; `None` when the
    /// panel shows no session yet, another backend, or a session whose model is not known.
    fn running_model(&self, choice: &BackendChoice, cx: &App) -> Option<String> {
        let session = self.session(cx)?;
        let chat = self.env.sessions.read(cx).session(session)?.chat.clone()?;
        let same = match choice {
            BackendChoice::Harness(kind) => chat.agent == kind.as_str(),
            BackendChoice::Provider(id) => {
                chat.agent == "assistant" && chat.provider_id.as_deref() == Some(id.as_str())
            }
        };
        (same && !chat.model.is_empty()).then_some(chat.model)
    }

    /// The header's backend and model: what the shown assistant runs on, opening the chooser.
    /// With nothing chosen (no choice of this window's, none in `daemon.toml`) the daemon picked
    /// the backend, and the label begins "Automatic · " so it does not read like a pinned
    /// choice. A session whose model is unknown (the harness's own default runs) says
    /// "Automatic" for the model.
    pub fn backend_label(&self, cx: &App) -> String {
        let workspace = match self.key(cx) {
            ScopeKey::Global => None,
            ScopeKey::Workspace(id) => Some(id),
        };
        let automatic = self.env.sessions.read(cx).assistant_is_automatic(workspace);
        self.session(cx)
            .and_then(|session| {
                let store = self.env.sessions.read(cx);
                let chat = store.session(session)?.chat.clone()?;
                let backend = if chat.agent == "assistant" {
                    chat.provider_id.unwrap_or_else(|| "provider".to_owned())
                } else {
                    harnesses::harness_name(&chat.agent)
                };
                Some(match (automatic, chat.model.is_empty()) {
                    (true, true) => format!("Automatic · {backend}"),
                    (true, false) => format!("Automatic · {backend} · {}", chat.model),
                    (false, true) => format!("{backend} · Automatic"),
                    (false, false) => format!("{backend} · {}", chat.model),
                })
            })
            .unwrap_or_else(|| "Choose backend".to_owned())
    }

    /// The header's backend·model chip: what the shown assistant runs on, cut with "…" to the
    /// room the sidebar leaves it. It opens the menu with Switch model… and the chooser.
    fn backend_chip(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let label = self.backend_label(cx);
        let enabled = self.connected(cx);
        let hover = theme.surface_hover;
        div()
            .id("assistant-backend")
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(4.0))
            .h(px(24.0))
            .px(px(8.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .text_size(px(tokens.secondary_font_size))
            .when(enabled, move |s| {
                s.cursor_pointer().hover(move |s| s.bg(hover))
            })
            .when(!enabled, |s| s.opacity(0.5))
            .when(enabled, |s| {
                s.on_click(cx.listener(|_, e: &gpui::ClickEvent, window, cx| {
                    window.dispatch_action(Box::new(ShowAssistantModelMenu(e.position())), cx);
                }))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(SharedString::from(label)),
            )
            .child(icon(Icon::ChevronDown, 12.0, theme.muted))
            .child(probe(&self.env.regions, Region::Assistant("backend")))
            .into_any_element()
    }

    /// The chooser: backends first, then the models of the one picked; a model (or the
    /// backend's default) is the choice.
    fn render_switcher(&self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (look.theme, look.tokens);
        let directory = self.env.harnesses.read(cx);
        let backend = self.switcher.as_ref().and_then(|s| s.backend.clone());
        let mut choices: Vec<(String, BackendChoice, String, bool, &'static str)> = Vec::new();
        for harness in HARNESSES {
            let kind = match harness.id {
                harnesses::OMP => AssistantBackend::Omp,
                harnesses::CLAUDE => AssistantBackend::Claude,
                _ => AssistantBackend::Codex,
            };
            let detection = directory.detection(harness.id);
            let detail = match &detection {
                Detection::NotFound(how) => how.clone(),
                _ => harnesses::login_summary(directory, harness.id),
            };
            let usable = !matches!(detection, Detection::NotFound(_));
            choices.push((
                format!("backend-{}", harness.id),
                BackendChoice::Harness(kind),
                detail,
                usable,
                match harness.id {
                    harnesses::OMP => "backend-omp",
                    harnesses::CLAUDE => "backend-claude",
                    _ => "backend-codex",
                },
            ));
        }
        for provider in &self.providers {
            choices.push((
                format!("backend-provider-{}", provider.id),
                BackendChoice::Provider(provider.id.clone()),
                provider.base_url.clone(),
                true,
                "backend-provider",
            ));
        }
        let rows: Vec<AnyElement> = choices
            .into_iter()
            .map(|(id, choice, detail, usable, region)| {
                let selected = backend.as_ref() == Some(&choice);
                let picked = choice.clone();
                div()
                    .id(SharedString::from(id))
                    .flex()
                    .flex_col()
                    .px(px(tokens.spacing * 2.0))
                    .py(px(tokens.spacing))
                    .rounded(px(tokens.radius))
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .when(usable, |s| {
                        s.cursor_pointer()
                            .hover(move |s| s.bg(theme.surface_hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.choose_backend(picked.clone(), cx);
                            }))
                    })
                    .when(!usable, |s| s.opacity(0.6))
                    .child(div().medium().child(SharedString::from(choice.label())))
                    .child(
                        div()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(SharedString::from(detail)),
                    )
                    .child(probe(&self.env.regions, Region::Assistant(region)))
                    .into_any_element()
            })
            .collect();
        let mut models: Vec<AnyElement> = Vec::new();
        if let Some(chosen) = backend.clone() {
            let default_choice = chosen.clone();
            // No stored model: the daemon picks one. Once the session exists it says which.
            let automatic = match self.running_model(&chosen, cx) {
                Some(model) => format!("Automatic · runs {model}"),
                None => "Automatic".to_owned(),
            };
            models.push(
                button(
                    "assistant-model-default",
                    automatic,
                    ButtonKind::Primary,
                    true,
                    look,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.use_backend(default_choice.clone(), None, cx);
                }))
                .child(probe(&self.env.regions, Region::Assistant("use-default")))
                .into_any_element(),
            );
            match self.switcher_models(cx) {
                Models::Loading => models.push(
                    div()
                        .text_color(theme.muted)
                        .child("Loading the models…")
                        .into_any_element(),
                ),
                Models::Failed(error) => models.push(
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(error))
                        .into_any_element(),
                ),
                Models::Ready(list) => {
                    let list: Vec<ModelInfo> = list.into_iter().filter(|m| !m.hidden).collect();
                    for (index, model) in list.into_iter().enumerate() {
                        let chosen = chosen.clone();
                        let id = model.id.clone();
                        models.push(
                            button(
                                format!("assistant-model-{index}"),
                                model.display_name.clone(),
                                ButtonKind::Ghost,
                                true,
                                look,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.use_backend(chosen.clone(), Some(id.clone()), cx);
                            }))
                            .into_any_element(),
                        );
                    }
                }
            }
        }
        div()
            .id("assistant-switcher")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 3.0))
            .child(ui::section_label(
                "What the assistant runs on",
                &theme,
                &tokens,
            ))
            .children(rows)
            .child(ui::section_label("Model", &theme, &tokens))
            .children(models)
            .into_any_element()
    }

    fn notice(
        &self,
        mascot: Mascot,
        title: &str,
        detail: Vec<String>,
        buttons: Vec<AnyElement>,
        look: &Look,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(tokens.spacing * 2.0))
            .p(px(tokens.spacing * 4.0))
            .child(ui::mascot(mascot, crate::theme::MASCOT_SIZE))
            .child(
                div()
                    .prose(tokens)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(title.to_owned())),
            )
            .children(detail.into_iter().map(|line| {
                div()
                    .max_w(px(520.0))
                    .prose(tokens)
                    .text_color(theme.muted)
                    .child(SharedString::from(line))
            }))
            .child(div().flex().gap(px(tokens.spacing * 2.0)).children(buttons))
            .into_any_element()
    }

    fn render_body(&mut self, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let key = self.key(cx);
        if matches!(self.state(key), Opened::Failed(Failure::Unconfigured(_)))
            && *self.env.harnesses.read(cx).listing() == Listing::Idle
        {
            self.env
                .harnesses
                .update(cx, |directory, cx| directory.refresh(cx));
        }
        let retry = button(
            "assistant-retry",
            "Retry",
            ButtonKind::Secondary,
            true,
            look,
        )
        .on_click(cx.listener(|this, _, _, cx| this.retry(cx)))
        .child(probe(&self.env.regions, Region::Assistant("retry")))
        .into_any_element();
        let models = button(
            "assistant-models",
            "Add a provider…",
            ButtonKind::Primary,
            true,
            look,
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(ManageProviders), cx))
        .child(probe(&self.env.regions, Region::Assistant("models")))
        .into_any_element();
        if self.switcher.is_some() && self.connected(cx) {
            return self.render_switcher(look, cx);
        }
        if !self.connected(cx) {
            return self.notice(
                Mascot::Sleep,
                "Not connected to hutand",
                vec!["The assistant opens as soon as the daemon is back.".to_owned()],
                Vec::new(),
                look,
            );
        }
        match self.state(key) {
            Opened::Idle | Opened::Opening => self.notice(
                Mascot::Thinking,
                "Opening the assistant…",
                Vec::new(),
                Vec::new(),
                look,
            ),
            Opened::Ready(session) => match self.chats.get(&session) {
                Some(chat) => div()
                    .size_full()
                    .child(chat.view.clone())
                    .into_any_element(),
                None => self.notice(
                    Mascot::Thinking,
                    "Opening the assistant…",
                    Vec::new(),
                    Vec::new(),
                    look,
                ),
            },
            Opened::Failed(Failure::Unconfigured(_)) => {
                let directory = self.env.harnesses.read(cx);
                let omp_ready = directory.detection(harnesses::OMP) == Detection::Installed
                    && !directory.logged_in(harnesses::OMP).is_empty();
                let can_choose = self
                    .env
                    .sessions
                    .read(cx)
                    .connection()
                    .supports_assistant_backend();
                let mut buttons: Vec<AnyElement> = Vec::new();
                if omp_ready && can_choose {
                    buttons.push(
                        button(
                            "assistant-use-omp",
                            "Use OMP",
                            ButtonKind::Primary,
                            true,
                            look,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.use_omp_everywhere(cx)))
                        .child(probe(&self.env.regions, Region::Assistant("use-omp")))
                        .into_any_element(),
                    );
                }
                buttons.push(models);
                buttons.push(retry);
                let detail = if can_choose {
                    "Answer through OMP when you are logged in to it (Settings → Accounts), or add a model provider with your own key. Nothing is sent anywhere until you choose."
                } else {
                    "This hutand is older than the app: restart it (Daemon → Restart) to choose OMP, Claude Code or Codex. A model provider works as before."
                };
                self.notice(
                    Mascot::Empty,
                    "Set up the assistant",
                    vec![detail.to_owned()],
                    buttons,
                    look,
                )
            }
            Opened::Failed(Failure::Unsupported) => self.notice(
                Mascot::Error,
                "This hutand has no assistant",
                vec![
                    "Restart the daemon (Daemon → Restart) to run the version of this app."
                        .to_owned(),
                ],
                vec![retry],
                look,
            ),
            Opened::Failed(Failure::Other(error)) => {
                // The configured backend failed to start: offer another one, not only a
                // provider (a harness backend has nothing to do with provider keys).
                let change = button(
                    "assistant-change-backend",
                    "Change backend…",
                    ButtonKind::Primary,
                    true,
                    look,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_switcher(cx)))
                .child(probe(
                    &self.env.regions,
                    Region::Assistant("change-backend"),
                ))
                .into_any_element();
                // A harness the daemon could not find: offer to install it from its source.
                let install = crate::harness_install::harness_of_missing(&error).map(|harness| {
                    button(
                        "assistant-install",
                        "Install…",
                        ButtonKind::Primary,
                        true,
                        look,
                    )
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(crate::workbench::actions::InstallHarness(
                                harness.to_owned(),
                            )),
                            cx,
                        );
                    })
                    .child(probe(&self.env.regions, Region::Assistant("install")))
                    .into_any_element()
                });
                let mut buttons: Vec<AnyElement> = install.into_iter().collect();
                buttons.push(change);
                buttons.push(retry);
                self.notice(
                    Mascot::Error,
                    "The assistant could not be opened",
                    vec![error],
                    buttons,
                    look,
                )
            }
        }
    }
}

impl Render for AssistantPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        self.env.regions.rendered(cx.entity_id());
        self.ensure_open(cx);
        self.ensure_policy(cx);
        if self.focus_wanted
            && let Some(chat) = self.session(cx).and_then(|s| self.chats.get(&s))
        {
            self.focus_wanted = false;
            let handle = chat.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
        let look = Look::of(cx);
        let header = self.render_header(&look, cx);
        let body = self.render_body(&look, cx);
        let (theme, tokens) = (look.theme, look.tokens);
        let notes: Vec<AnyElement> = [
            self.mode_note
                .as_ref()
                .map(|note| (note, theme.muted, "mode-note")),
            self.error
                .as_ref()
                .map(|error| (error, theme.danger, "error")),
        ]
        .into_iter()
        .flatten()
        .map(|(text, color, region)| {
            div()
                .relative()
                .flex_none()
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .text_color(color)
                .text_size(px(tokens.secondary_font_size))
                .child(SharedString::from(text.clone()))
                .child(probe(&self.env.regions, Region::Assistant(region)))
                .into_any_element()
        })
        .collect();
        let rail = (self.rail_open && self.connected(cx)).then(|| self.render_rail(&look, cx));
        let asks = self.render_chat_asks(&look, cx);
        div()
            .id("assistant-panel")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .text_color(theme.fg)
            .text_size(px(tokens.font_size))
            .child(header)
            .children(notes)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .relative()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .justify_center()
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .w_full()
                                    .max_w(px(CONVERSATION_WIDTH))
                                    .h_full()
                                    .children(asks)
                                    .child(div().relative().flex_1().min_h_0().child(body)),
                            ),
                    )
                    .children(rail),
            )
    }
}
