//! Workbench behavior around agent chats (M4.4): chat tabs, the Switch model dialog, global
//! approvals, and "Send to agent" from terminal blocks.
//!
//! A chat tab is a [`ChatView`] per `(pane, session)` over one [`ChatModel`] per session, held
//! while some tab shows the session (the transcript is read again from the daemon's journal when
//! a tab is opened later). The daemon owns everything else: the conversation, the approvals,
//! the agent process.

use std::collections::HashSet;

use gpui::{
    AnyElement, AppContext as _, Context, ElementId, Entity, Focusable as _, InteractiveElement,
    IntoElement, MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::*, px,
};
use hutan_core::agent::ModelInfo;
use hutan_core::{ApprovalId, SessionId};
use hutan_protocol::blocks::BlockPart;
use hutan_protocol::messages::SessionInfo;

use super::actions::palette;
use super::chat::{ButtonKind, ChatModel, ChatView, Chip, ContextChip, Look, button, cut_to};
use super::harnesses::{self, HARNESSES};
use super::launcher::LauncherPreset;
use super::model::{FocusTarget, ItemRef, PaneId};
use super::panels::AssistantPanel;
use super::render::probe;
use super::terminals::session_of_item;
use super::ui::{self, StyledExt as _};
use super::view::{Region, Workbench};
use super::vim_ui::OverlayField;
use crate::settings::ChatSettings;
use crate::theme::DensityExt as _;

pub use super::harnesses::CODEX;

/// The models a picker may offer once the daemon answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Models {
    Loading,
    Ready(Vec<ModelInfo>),
    Failed(String),
}

/// The Switch model dialog of a running chat: the models its harness (or, for a direct chat,
/// its provider) offers. Starting an agent is the Start… sheet (`launcher.rs`).
#[derive(Debug, Clone)]
pub struct AgentPicker {
    pub models: Models,
    /// Index into the models that are shown (the hidden ones are not).
    pub selected: usize,
    /// `chat.set_model` is in flight.
    pub creating: bool,
    pub error: Option<super::providers::ProviderFailure>,
    pub provider_id: Option<String>,
    pub model_query: String,
    /// The chat whose model the dialog switches.
    pub switch_session: SessionId,
    pub direct: bool,
    /// The harness of the chat (`omp`, `claude`); a direct chat has none (`provider_id` says
    /// what).
    pub agent: String,
}

impl AgentPicker {
    /// The models the dialog lists. OMP's are grouped by provider (stable within a group).
    pub fn shown(&self) -> Vec<&ModelInfo> {
        match &self.models {
            Models::Ready(models) => {
                let query = self.model_query.to_lowercase();
                let mut shown: Vec<&ModelInfo> = models
                    .iter()
                    .filter(|m| {
                        !m.hidden
                            && (query.is_empty()
                                || m.id.to_lowercase().contains(&query)
                                || m.display_name.to_lowercase().contains(&query))
                    })
                    .collect();
                if self.provider_id.is_none() {
                    let agent = self.agent.as_str();
                    shown.sort_by_key(|m| harnesses::model_group(agent, m));
                }
                shown
            }
            _ => Vec::new(),
        }
    }

    /// The model a new chat would start with.
    pub fn chosen(&self) -> Option<&ModelInfo> {
        self.shown().get(self.selected).copied()
    }
    // A harness that offers no model never reaches the dialog: its chat keeps its default.
}

impl Workbench {
    // ---- chat tabs -------------------------------------------------------------------------

    /// Every `(pane, session)` that has a chat tab, in tree order.
    fn open_chat_items(&self) -> Vec<(PaneId, SessionId)> {
        self.state
            .center
            .panes()
            .into_iter()
            .flat_map(|pane| {
                pane.items.iter().filter_map(move |item| match item {
                    ItemRef::Chat { session } => {
                        session.parse().ok().map(|session| (pane.id, session))
                    }
                    _ => None,
                })
            })
            .collect()
    }

    /// Creates the models and views the open chat tabs need and drops the ones no tab shows.
    pub(crate) fn sync_chats(&mut self, cx: &mut Context<Self>) {
        let items = self.open_chat_items();
        let mut wanted: HashSet<SessionId> = items.iter().map(|(_, session)| *session).collect();
        // Chats in the overview's stash keep their transcript.
        if let Some(saved) = &self.state.stashed {
            for pane in saved.center.panes() {
                for item in &pane.items {
                    if let ItemRef::Chat { session } = item
                        && let Ok(session) = session.parse()
                    {
                        wanted.insert(session);
                    }
                }
            }
        }
        self.chat_models
            .retain(|session, _| wanted.contains(session));
        let keep: HashSet<(PaneId, SessionId)> = items.into_iter().collect();
        let gone: Vec<(PaneId, SessionId)> = self
            .chats
            .keys()
            .filter(|key| !keep.contains(*key))
            .copied()
            .collect();
        for key in gone {
            if let Some(view) = self.chats.remove(&key) {
                // Focus may still sit on it; `rescue_orphaned_focus` checks before it is dropped.
                self.pruned_chats.push(view);
            }
        }
        for session in wanted {
            self.ensure_chat_model(session, cx);
        }
    }

    pub(crate) fn ensure_chat_model(
        &mut self,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Entity<ChatModel> {
        if let Some(model) = self.chat_models.get(&session) {
            return model.clone();
        }
        let connection = self.sessions.read(cx).connection().clone();
        let model = cx.new(|cx| ChatModel::new(session, connection, cx));
        self.chat_models.insert(session, model.clone());
        model
    }

    /// The view of `session` in `pane`, created on first use.
    pub(crate) fn chat_view(
        &mut self,
        pane: PaneId,
        session: SessionId,
        cx: &mut Context<Self>,
    ) -> Entity<ChatView> {
        if let Some(view) = self.chats.get(&(pane, session)) {
            return view.clone();
        }
        let model = self.ensure_chat_model(session, cx);
        let sessions = self.sessions.clone();
        let settings = self.settings.clone();
        let regions = self.regions.clone();
        let view = cx.new(|cx| ChatView::new(session, model, sessions, settings, regions, cx));
        self.chats.insert((pane, session), view.clone());
        view
    }

    /// The view of `session` in any pane (tests, inspection).
    pub fn chat_view_of(&self, session: SessionId) -> Option<Entity<ChatView>> {
        self.chats
            .iter()
            .find(|((_, s), _)| *s == session)
            .map(|(_, view)| view.clone())
    }

    /// The transcript and composer of `session`, while a tab holds it.
    pub fn chat_model_of(&self, session: SessionId) -> Option<Entity<ChatModel>> {
        self.chat_models.get(&session).cloned()
    }

    /// The view of the focused pane's active tab, if it is a chat.
    pub fn focused_chat_view(&self) -> Option<Entity<ChatView>> {
        let FocusTarget::Pane(_) = self.state.focus else {
            return None;
        };
        let pane = self.state.active_pane_id();
        match self.state.active_pane()?.active_item()? {
            ItemRef::Chat { session } => {
                let session = session.parse().ok()?;
                self.chats.get(&(pane, session)).cloned()
            }
            _ => None,
        }
    }

    /// Whether the session is an agent chat (it opens as a chat tab).
    pub(crate) fn is_chat(&self, session: SessionId, cx: &gpui::App) -> bool {
        self.sessions
            .read(cx)
            .session(session)
            .is_some_and(|info| info.chat.is_some())
    }

    /// Body of a chat tab.
    pub(crate) fn render_chat_item(
        &mut self,
        pane: PaneId,
        session: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Ok(id) = session.parse::<SessionId>() else {
            return self.placeholder_body(format!("Invalid session id {session}"), cx);
        };
        self.rendered_chats.insert((pane, id));
        let view = self.chat_view(pane, id, cx);
        super::render::cached_view(&self.regions, view)
    }

    // ---- Start… and the Switch model dialog --------------------------------------------------

    /// The open Switch model dialog, if any.
    pub fn agent_picker(&self) -> Option<&AgentPicker> {
        self.agent_picker.as_ref()
    }

    /// ⌘⇧T and ⌘K "Start Agent or Task…": the Start… sheet in the project in focus.
    pub fn new_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_launcher(LauncherPreset::default(), window, cx);
    }

    /// ⌘K "Chat: New Direct Chat…": the Start… sheet on a direct model.
    pub fn new_direct_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_launcher(
            LauncherPreset {
                direct: true,
                ..LauncherPreset::default()
            },
            window,
            cx,
        );
    }

    /// The dialog that switches the model of a running chat: `agent` is its harness, none for a
    /// direct chat (which can change provider too).
    fn open_model_switch_dialog(
        &mut self,
        session: SessionId,
        agent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (connected, supported) = {
            let store = self.sessions.read(cx);
            (
                store.connection_state() == hutan_client::ConnectionState::Connected,
                store.connection().supports_chat(),
            )
        };
        if !connected {
            self.set_notice("Not connected to hutand", cx);
            return;
        }
        if !supported {
            self.set_notice(
                "This hutand does not run agent chats; restart it (Daemon → Restart)",
                cx,
            );
            return;
        }
        self.palette = None;
        self.picker = None;
        self.context_menu = None;
        self.launcher = None;
        let direct = agent.is_none();
        self.agent_picker = Some(AgentPicker {
            models: Models::Loading,
            selected: 0,
            creating: false,
            error: None,
            provider_id: None,
            model_query: String::new(),
            switch_session: session,
            direct,
            agent: agent.unwrap_or_default(),
        });
        window.focus(&self.agent_focus, cx);
        self.edits.agent.reset();
        if direct {
            self.refresh_providers(cx);
        } else {
            self.load_harness_models(cx);
        }
        cx.notify();
    }

    /// The model dialog of a running harness chat (OMP, Claude Code): the models its harness
    /// offers; the daemon switches them between turns where the harness allows it.
    pub(crate) fn open_harness_model_switch(
        &mut self,
        session: SessionId,
        agent: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_model_switch_dialog(session, Some(agent), window, cx);
    }

    /// The model dialog of a running direct chat: provider and model.
    pub(crate) fn open_direct_model_switch(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_model_switch_dialog(session, None, window, cx);
    }

    /// Whether the daemon can start chats with `harness`: Codex always, the ACP harnesses when
    /// it advertises them.
    pub(crate) fn harness_available(&self, harness: &str, cx: &gpui::App) -> bool {
        harness == CODEX || self.sessions.read(cx).connection().supports_harness_chat()
    }

    /// The harness the Start… sheet preselects before it knows what is installed: the first of
    /// OMP, Claude Code, Codex that the daemon can run and has not found missing.
    pub(crate) fn default_harness(&self, cx: &gpui::App) -> String {
        let directory = self.env.harnesses.read(cx);
        HARNESSES
            .iter()
            .find(|h| self.harness_available(h.id, cx) && directory.startable(h.id))
            .map_or(CODEX, |h| h.id)
            .to_owned()
    }

    /// Asks the harness of the open Switch model dialog for its models.
    fn load_harness_models(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.agent_picker else {
            return;
        };
        if picker.direct {
            return;
        }
        picker.models = Models::Loading;
        picker.selected = 0;
        let agent = picker.agent.clone();
        self.env
            .harnesses
            .update(cx, |directory, cx| directory.load_models(&agent, cx));
    }

    /// What the directory learned (accounts, models) reaches the open Start… sheet and the
    /// Switch model dialog.
    pub(crate) fn on_harnesses_changed(&mut self, cx: &mut Context<Self>) {
        self.auto_install_step(cx);
        self.integration_statuses_step(cx);
        self.launcher_on_harnesses_changed(cx);
        let Some(picker) = &self.agent_picker else {
            return;
        };
        if picker.direct || picker.provider_id.is_some() {
            return;
        }
        let models = self.env.harnesses.read(cx).models(&picker.agent).cloned();
        if let Some(models) = models
            && let Some(picker) = &mut self.agent_picker
            && picker.models != models
        {
            picker.models = models;
            picker.selected = picker
                .shown()
                .iter()
                .position(|model| model.is_default)
                .unwrap_or(0);
        }
        cx.notify();
    }

    pub fn close_agent_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.agent_picker.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn agent_picker_select(&mut self, next: bool, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.agent_picker {
            let last = picker.shown().len().saturating_sub(1);
            picker.selected = if next {
                (picker.selected + 1).min(last)
            } else {
                picker.selected.saturating_sub(1)
            };
            cx.notify();
        }
    }

    /// Picks model row `index` (a click).
    pub fn agent_picker_choose(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(picker) = &mut self.agent_picker
            && index < picker.shown().len()
        {
            picker.selected = index;
            cx.notify();
        }
    }

    /// Switches the chat to the model the dialog shows.
    pub fn agent_picker_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.agent_picker else {
            return;
        };
        if picker.creating {
            return;
        }
        let Some(model) = picker.chosen().map(|m| m.id.clone()) else {
            return;
        };
        picker.creating = true;
        picker.error = None;
        let provider_id = picker.provider_id.clone();
        let session = picker.switch_session;
        self.confirm_model_switch(session, provider_id, model, window, cx);
    }

    pub(crate) fn render_agent_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.agent_picker.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.surface_hover;
        let look = Look::of(cx);
        // A direct chat can move to another provider; a harness chat keeps its harness.
        let providers: Vec<AnyElement> = if picker.direct {
            self.providers
                .providers
                .iter()
                .map(|provider| {
                    let id = provider.id.clone();
                    let selected = picker.provider_id.as_deref() == Some(id.as_str());
                    button(
                        format!("agent-provider-{id}"),
                        format!("Direct model · {}", provider.name),
                        ButtonKind::Secondary,
                        !picker.creating,
                        &look,
                    )
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.choose_direct_provider(&id, cx)),
                    )
                    .into_any_element()
                })
                .collect()
        } else {
            Vec::new()
        };
        let shown = picker.shown();
        let mut models: Vec<AnyElement> = Vec::new();
        let mut group: Option<Option<String>> = None;
        for (index, model) in shown.iter().enumerate() {
            let model_group = harnesses::model_group(&picker.agent, model)
                .filter(|_| picker.provider_id.is_none());
            // OMP's models come grouped by provider; a header opens each group.
            if model_group.is_some() && group.as_ref() != Some(&model_group) {
                models.push(
                    ui::section_label(model_group.clone().unwrap_or_default(), &theme, &tokens)
                        .px(px(tokens.spacing * 2.0))
                        .pt(px(tokens.spacing))
                        .into_any_element(),
                );
            }
            group = Some(model_group);
            let selected = index == picker.selected;
            models.push(
                div()
                    .id(ElementId::from(SharedString::from(format!(
                        "agent-model-{index}"
                    ))))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius))
                    .cursor_pointer()
                    .when(selected, |s| s.bg(theme.accent_soft))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.agent_picker_choose(index, cx);
                    }))
                    .child(
                        div()
                            .w(px(14.0))
                            .text_color(theme.muted)
                            .child(if selected { "●" } else { "○" }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(SharedString::from(model.display_name.clone())),
                    )
                    .when(model.is_default, |s| {
                        s.child(div().text_color(theme.muted).child("default"))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .child(SharedString::from(model.description.clone())),
                    )
                    .child(probe(&self.regions, Region::AgentModel(index)))
                    .into_any_element(),
            );
        }
        let plain = |text: &'static str, color: gpui::Hsla| {
            div()
                .px(px(tokens.spacing * 2.0))
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        let note: Option<AnyElement> = match (&picker.models, &picker.error) {
            (_, Some(error)) => Some(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .child(self.render_provider_failure(error, cx))
                    .into_any_element(),
            ),
            (Models::Loading, _) => Some(plain("Loading the models…", theme.muted)),
            (Models::Failed(error), _) => Some(
                div()
                    .px(px(tokens.spacing * 2.0))
                    .child(self.render_models_failure(error, cx))
                    .into_any_element(),
            ),
            (Models::Ready(_), _) if picker.shown().is_empty() => {
                Some(plain("The agent offers no models.", theme.muted))
            }
            _ => None,
        };
        let can_start = picker.chosen().is_some() && !picker.creating;
        let section = |text: &'static str| {
            div()
                .px(px(tokens.spacing * 2.0))
                .pt(px(tokens.spacing * 2.0))
                .text_size(px(tokens.section_font_size))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.muted)
                .child(text)
        };
        Some(
            div()
                .id("agent-picker-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(80.0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_agent_picker(window, cx)),
                )
                .child(
                    div()
                        .id("agent-picker")
                        .key_context("AgentPicker")
                        .track_focus(&self.agent_focus)
                        .w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .popover(&theme, &tokens)
                        .p(px(tokens.spacing * 2.0))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                            // Typed text arrives through the field's platform text input.
                            if this.overlay_key(OverlayField::Agent, event, cx) {
                                cx.stop_propagation();
                            }
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectNext, _, cx| {
                            this.agent_picker_select(true, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectPrev, _, cx| {
                            this.agent_picker_select(false, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::Confirm, window, cx| {
                            this.agent_picker_confirm(window, cx);
                        }))
                        .child(
                            div()
                                .px(px(tokens.spacing * 2.0))
                                .h(px(tokens.sidebar_row + 6.0))
                                .flex()
                                .items_center()
                                .border_b_1()
                                .border_color(theme.border)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("Switch model"),
                        )
                        .children(providers)
                        .child(section("Model"))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(tokens.spacing))
                                .child(div().text_color(theme.muted).child("Filter:"))
                                .children(self.overlay_parts(
                                    OverlayField::Agent,
                                    "Type to filter models",
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .id("agent-model-list")
                                .max_h(px(260.0))
                                .overflow_y_scroll()
                                .children(models),
                        )
                        .children(note)
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(tokens.spacing * 2.0))
                                .pt(px(tokens.spacing * 2.0))
                                .child(
                                    button(
                                        "agent-cancel",
                                        "Cancel",
                                        ButtonKind::Ghost,
                                        true,
                                        &look,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.close_agent_picker(window, cx);
                                        },
                                    )),
                                )
                                .child(
                                    button(
                                        "agent-start",
                                        if picker.creating {
                                            "Switching…"
                                        } else {
                                            "Switch ↩"
                                        },
                                        ButtonKind::Primary,
                                        can_start,
                                        &look,
                                    )
                                    .when(can_start, |s| {
                                        s.on_click(cx.listener(|this, _, window, cx| {
                                            this.agent_picker_confirm(window, cx);
                                        }))
                                    })
                                    .child(probe(&self.regions, Region::AgentPicker("start"))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    // ---- approvals -------------------------------------------------------------------------

    /// Answers a pending request from outside its chat (the inspector, the review picker).
    pub fn answer_approval(
        &mut self,
        approval: ApprovalId,
        option: String,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.respond_approval(approval, option, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.set_notice(format!("Could not answer: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// Shows the permission request waiting longest: its chat, where the card is.
    pub fn show_oldest_approval(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self
            .sessions
            .read(cx)
            .approvals()
            .first()
            .map(|approval| approval.session_id);
        match session {
            Some(session) => self.open_session(session, window, cx),
            None => self.set_notice("No approvals are waiting", cx),
        }
    }

    /// Reveals the inspector when a permission request arrives that this window has not seen
    /// (`panels.inspector.reveal_on_approval`).
    pub(crate) fn on_approvals_changed(&mut self, cx: &mut Context<Self>) {
        let seen: HashSet<ApprovalId> = self
            .sessions
            .read(cx)
            .approvals()
            .iter()
            .map(|a| a.id)
            .collect();
        let arrived = seen.iter().any(|id| !self.known_approvals.contains(id));
        let first_look = !self.sessions.read(cx).synced();
        self.known_approvals = seen;
        if !arrived || first_look {
            return;
        }
        let settings = self
            .settings
            .get::<crate::settings::InspectorPanelSettings>();
        if settings.reveal_on_approval {
            self.open_panel("inspector", cx);
        }
    }

    // ---- send to agent ---------------------------------------------------------------------

    /// "Send to agent" of a terminal block: its command and output become a chip in the
    /// composer of the last chat the user looked at (the assistant the Assistant panel shows,
    /// once they opened it after the last chat tab), or of the one they pick. An assistant takes
    /// the block as a structured attachment that its daemon reads (`shell_block`), never as text
    /// read here; the menu offers "Send to Assistant" (the assistant of the terminal's
    /// workspace, opened on demand) when none of the running chats is one.
    pub(crate) fn send_block_to_agent(
        &mut self,
        session: SessionId,
        block: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let candidates: Vec<SessionInfo> = self
            .sessions
            .read(cx)
            .sessions()
            .iter()
            .filter(|s| s.chat.is_some() && s.state.is_live())
            .cloned()
            .collect();
        let shown_assistant = self
            .assistant_last
            .then(|| {
                self.panel::<AssistantPanel>("assistant")
                    .and_then(|panel| panel.read(cx).session(cx))
            })
            .flatten();
        let last = shown_assistant
            .or(self.last_chat)
            .filter(|last| candidates.iter().any(|c| c.id == *last));
        let target = match (last, candidates.as_slice()) {
            (Some(last), _) => Some(last),
            (None, [only]) => Some(only.id),
            (None, []) => {
                self.set_notice(
                    "No agent chat is running: start one with ⌘⇧T, or open Mawas from the command palette",
                    cx,
                );
                return;
            }
            (None, _) => None,
        };
        match target {
            Some(chat) => self.attach_block(session, block, chat, window, cx),
            None => {
                let mut entries: Vec<_> = candidates
                    .iter()
                    .map(|info| {
                        let chat = info.id;
                        Self::entry(
                            format!("Send to {}", info.title),
                            move |this, window, cx| {
                                this.attach_block(session, block, chat, window, cx);
                            },
                        )
                    })
                    .collect();
                let has_assistant = candidates
                    .iter()
                    .any(|c| c.chat.as_ref().is_some_and(|chat| chat.is_assistant()));
                if self.sessions.read(cx).connection().supports_assistant() && !has_assistant {
                    entries.push(Self::entry(
                        "Send to Mawas".to_owned(),
                        move |this, window, cx| {
                            this.send_block_to_assistant(session, block, window, cx);
                        },
                    ));
                }
                self.open_menu(window.mouse_position(), entries, window, cx);
            }
        }
    }

    /// Fetches the block's text from the daemon (cut at `chat.attach_max_bytes`) and attaches it
    /// to `chat`'s composer, opening the chat when no tab shows it.
    fn attach_block(
        &mut self,
        session: SessionId,
        block: u64,
        chat: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self
            .sessions
            .read(cx)
            .session(session)
            .and_then(|info| info.terminal_id)
        else {
            return;
        };
        let (command, exit_code) = self.block_facts(session, block, cx);
        if self.is_assistant(chat, cx) {
            let title = self
                .sessions
                .read(cx)
                .session(session)
                .map_or_else(|| "terminal".to_owned(), |info| info.title.clone());
            let chip = ContextChip::block(&title, terminal, block, command.as_deref(), exit_code);
            self.attach_to_assistant(chat, chip, window, cx);
            return;
        }
        let max = self.settings.get::<ChatSettings>().attach_max_bytes;
        let max = u32::try_from(max).unwrap_or(u32::MAX);
        let connection = self.sessions.read(cx).connection().clone();
        let model = self.ensure_chat_model(chat, cx);
        // The chat is shown so the user sees where the context went.
        self.open_session(chat, window, cx);
        cx.spawn(async move |this, cx| {
            let output = connection
                .terminal_block_text(terminal, block, BlockPart::Output, max)
                .await;
            let command_text = match command {
                Some(command) => Some(command),
                None => connection
                    .terminal_block_text(terminal, block, BlockPart::Command, 16 * 1024)
                    .await
                    .ok()
                    .map(|answer| answer.text),
            };
            match output {
                Ok(output) => {
                    let (text, cut) = cut_to(&output.text, max as usize);
                    let chip = Chip::from_block(
                        command_text.as_deref(),
                        exit_code,
                        &text,
                        output.truncated || cut,
                    );
                    model.update(cx, |model, cx| model.attach(chip, cx));
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.set_notice(format!("Could not read the block: {error}"), cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// Remembers which chat the user looked at last (the target of "Send to agent").
    pub(crate) fn note_focused_chat(&mut self, cx: &gpui::App) {
        if let Some(session) = self.focused_session()
            && self.is_chat(session, cx)
        {
            self.last_chat = Some(session);
            self.assistant_last = false;
        }
    }

    /// Focuses the chat view of the focused pane's active tab.
    pub(crate) fn focus_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(item) = self
            .state
            .active_pane()
            .and_then(|pane| pane.active_item())
            .cloned()
        else {
            return false;
        };
        let Some(session) = session_of_item(&item).filter(|_| matches!(item, ItemRef::Chat { .. }))
        else {
            return false;
        };
        let pane = self.state.active_pane_id();
        let view = self.chat_view(pane, session, cx);
        window.focus(&view.focus_handle(cx), cx);
        true
    }
}
