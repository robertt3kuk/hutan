//! [`ChatView`]: the chat tab. A header (who, in what state), the virtualized transcript, an
//! optional banner, and the composer. Cards are drawn by `cards.rs`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, FollowMode, InteractiveElement,
    IntoElement, KeyDownEvent, ListAlignment, ListOffset, ListState, MouseButton, ParentElement,
    Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div,
    list, prelude::*, px,
};
use hutan_chat_view::markdown::Block;
use hutan_client::ConnectionState;
use hutan_core::{ApprovalId, SessionId, SessionState};
use hutan_protocol::messages::SessionInfo;
use hutan_settings::SettingsStore;

use super::attach::{Attaching, Stage};
use super::context::{is_gateway_chat, is_harness_chat};
use super::input::TextInput;
use super::markdown_view::Look;
use super::model::{ChatModel, LoadStatus};
use super::widgets::{ButtonKind, button, round_button};
use crate::sessions::SessionsStore;
use crate::settings::{ChatSettings, SendKey};
use crate::workbench::actions::{self, RestartSessionById};
use crate::workbench::harnesses::harness_name;
use crate::workbench::panels::{kind_icon, recovery_chip, state_dot, tone_color};
use crate::workbench::render::probe;
use crate::workbench::send_key::{send_key_control, stop_asking, write_send_key};
use crate::workbench::text_field::{EditableText, FieldRef, FieldState, edit_keys};
use crate::workbench::ui::{self, Mascot, StyledExt as _};
use crate::workbench::view::Region;
use crate::workbench::vim_ui::{self, CaretShape, VimConfig};

pub(crate) use crate::workbench::regions::Regions;

/// Key context of the chat view's root element, for keymap bindings.
pub const KEY_CONTEXT: &str = "HutanChat";

/// Tallest the composer's text grows before it scrolls.
const COMPOSER_MAX_HEIGHT: f32 = 220.0;
/// Diameter of the composer's round send / stop button.
const SEND_SIZE: f32 = 32.0;

pub struct ChatView {
    pub(super) session: SessionId,
    pub(super) model: Entity<ChatModel>,
    pub(super) sessions: Entity<SessionsStore>,
    pub(super) settings: SettingsStore,
    pub(super) regions: Regions,
    focus_handle: FocusHandle,
    pub(super) list: ListState,
    seen_epoch: u64,
    /// `Transcript::prepended` as of the last sync.
    synced_front: u64,
    synced_len: usize,
    synced_rev: u64,
    /// Things the user opened or closed, against each card's default: `(entry id, part)`.
    pub(super) toggled: HashSet<(u64, u32)>,
    /// Parsed markdown by entry id, with the entry revision it was parsed at.
    pub(super) parsed: HashMap<u64, (u64, Rc<Vec<Block>>)>,
    /// Cards built and markdown parsed so far (tests: a frame builds what is in view and parses
    /// each message revision once).
    pub(super) entries_built: usize,
    pub(super) markdown_parsed: usize,
    /// Requests waiting for an answer, oldest first (the keys 1–4 answer the first).
    pub(super) pending: Vec<ApprovalId>,
    /// What the user has typed or chosen in each question card (protocol 0.24).
    pub(super) forms: HashMap<hutan_core::QuestionId, super::question::QuestionForm>,
    /// The question whose card has the keyboard: typing goes to its focused field, and 1–4 never
    /// answer anything while it has it.
    pub(super) question_focus: Option<hutan_core::QuestionId>,
    /// The Attach menu or form of an assistant chat.
    pub(super) attach: Option<Attaching>,
    /// The composer has had the keyboard: from then on it may show the first-run question
    /// about the send key.
    send_key_armed: bool,
    _subscriptions: Vec<Subscription>,
}

impl std::fmt::Debug for ChatView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatView")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Whether the composer may be used, and why not.
#[derive(Debug, Clone, Default)]
struct Gate {
    /// A turn is in progress (the interrupt button is offered, sending is not).
    running: bool,
    /// Why nothing can be sent now.
    blocked: Option<SharedString>,
    /// The session ended: it can be restarted.
    restartable: bool,
}

impl ChatView {
    pub fn new(
        session: SessionId,
        model: Entity<ChatModel>,
        sessions: Entity<SessionsStore>,
        settings: SettingsStore,
        regions: Regions,
        cx: &mut Context<Self>,
    ) -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(800.0));
        list.set_follow_mode(FollowMode::Tail);
        // Scrolling to the first entry asks for the page before it, until the journal has no
        // more (the banner's "Load earlier" does the same for a list that fits the window).
        let earlier = model.clone();
        list.set_scroll_handler(move |event, _, cx| {
            if event.visible_range.start == 0 && !event.is_following_tail {
                let earlier = earlier.clone();
                // Updated once the scroll has been handled: the list is in the middle of it.
                cx.defer(move |cx| earlier.update(cx, |model, cx| model.load_earlier(cx)));
            }
        });
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
        ];
        Self {
            session,
            model,
            sessions,
            settings,
            regions,
            focus_handle: cx.focus_handle(),
            list,
            seen_epoch: 0,
            synced_front: 0,
            synced_len: 0,
            synced_rev: 0,
            toggled: HashSet::new(),
            parsed: HashMap::new(),
            entries_built: 0,
            markdown_parsed: 0,
            pending: Vec::new(),
            forms: HashMap::new(),
            question_focus: None,
            attach: None,
            send_key_armed: false,
            _subscriptions: subscriptions,
        }
    }

    /// Transcript cards built so far.
    pub fn entries_rendered(&self) -> usize {
        self.entries_built
    }

    /// Markdown texts parsed so far.
    pub fn markdown_parses(&self) -> usize {
        self.markdown_parsed
    }

    pub fn session_id(&self) -> SessionId {
        self.session
    }

    /// Whether the composer has the keyboard focus.
    pub fn focus_handle_is_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window)
    }

    pub fn model(&self) -> &Entity<ChatModel> {
        &self.model
    }

    /// The id of the transcript entry at the top of the list, with how far into it the list is
    /// scrolled: what "keep the reader's place" keeps when earlier entries arrive.
    pub fn scroll_anchor(&self, cx: &App) -> Option<(u64, Pixels)> {
        let top = self.list.logical_scroll_top();
        let model = self.model.read(cx);
        let entries = model.transcript().entries();
        entries
            .get(top.item_ix)
            .map(|entry| (entry.id, top.offset_in_item))
    }

    /// Whether the list follows the end of the transcript (the reader has not scrolled away).
    pub fn following_tail(&self) -> bool {
        self.list.is_following_tail()
    }

    /// Brings the list's item heights in line with the transcript: items whose entry changed are
    /// measured again, new ones are added; a transcript that was replaced starts a new list.
    fn sync_list(&mut self, cx: &App) {
        let transcript = self.model.read(cx).transcript();
        let len = transcript.len();
        if transcript.epoch() != self.seen_epoch || len < self.synced_len {
            self.list.reset(len);
            self.parsed.clear();
            self.seen_epoch = transcript.epoch();
            self.synced_front = transcript.prepended();
        } else {
            // Earlier entries went in front: their slots go in front of the list's, and the
            // position the reader was at stays on the same entry (the list moves its own
            // scroll position with the splice; one that was never scrolled sits at the first
            // item, which is now an older one).
            let added = (transcript.prepended() - self.synced_front) as usize;
            if added > 0 {
                let top = self.list.logical_scroll_top();
                self.list.splice(0..0, added);
                self.synced_len += added;
                self.synced_front = transcript.prepended();
                if !self.list.is_following_tail() {
                    self.list.scroll_to(ListOffset {
                        item_ix: top.item_ix + added,
                        offset_in_item: top.offset_in_item,
                    });
                }
            }
            for (at, entry) in transcript
                .entries()
                .iter()
                .enumerate()
                .take(self.synced_len)
            {
                if entry.rev > self.synced_rev {
                    self.list.splice(at..at + 1, 1);
                }
            }
            if len > self.synced_len {
                self.list
                    .splice(self.synced_len..self.synced_len, len - self.synced_len);
            }
        }
        self.synced_len = len;
        self.synced_rev = transcript.rev();
    }

    /// Flips a part of an entry (a diff, an output, reasoning) against its default.
    pub(super) fn toggle(&mut self, index: usize, entry: u64, part: u32, cx: &mut Context<Self>) {
        if !self.toggled.remove(&(entry, part)) {
            self.toggled.insert((entry, part));
        }
        // The card grows (or shrinks) where it is; following the tail would scroll it away from
        // under the pointer. Scrolling back to the end follows again.
        self.list.pause_following_tail();
        self.list.splice(index..index + 1, 1);
        cx.notify();
    }

    /// Whether `part` of `entry` is open: `default`, unless the user flipped it.
    pub(super) fn open(&self, entry: u64, part: u32, default: bool) -> bool {
        default != self.toggled.contains(&(entry, part))
    }

    // ---- state -----------------------------------------------------------------------------

    fn info(&self, cx: &App) -> Option<SessionInfo> {
        self.sessions.read(cx).session(self.session).cloned()
    }

    fn gate(&self, info: Option<&SessionInfo>, cx: &App) -> Gate {
        let store = self.sessions.read(cx);
        let transcript = self.model.read(cx).transcript();
        let Some(info) = info else {
            return Gate {
                blocked: Some("This session no longer exists. Close the tab.".into()),
                ..Gate::default()
            };
        };
        let mut gate = Gate::default();
        match &info.state {
            SessionState::Starting => gate.blocked = Some("The agent is starting…".into()),
            SessionState::Running | SessionState::Idle | SessionState::WaitingApproval => {
                gate.running = info
                    .chat
                    .as_ref()
                    .map_or(transcript.turn_active(), |chat| chat.turn_active);
            }
            SessionState::Exited { .. } | SessionState::Failed { .. } | SessionState::Stopped => {
                gate.blocked = Some("The session ended.".into());
                gate.restartable = true;
            }
        }
        if gate.blocked.is_none() && store.connection_state() != ConnectionState::Connected {
            gate.blocked = Some("Reconnecting to hutand…".into());
        }
        gate
    }

    /// Why nothing can be sent right now (the session ended, it is starting, the connection is
    /// down); `None` when the composer works.
    pub fn blocked_reason(&self, cx: &App) -> Option<String> {
        let info = self.info(cx);
        self.gate(info.as_ref(), cx).blocked.map(|r| r.to_string())
    }

    /// Whether the composer's Send would do something now.
    pub fn can_send(&self, cx: &App) -> bool {
        let info = self.info(cx);
        let gate = self.gate(info.as_ref(), cx);
        let model = self.model.read(cx);
        // An attachment can be the whole message, unless one of them cannot be sent.
        gate.blocked.is_none()
            && !gate.running
            && !model.sending()
            && (!model.draft_message().is_empty() || model.has_context())
            && self.refused_attachment(cx).is_none()
    }

    // ---- commands --------------------------------------------------------------------------

    pub fn send(&mut self, cx: &mut Context<Self>) -> bool {
        // ⌘↩ is the composer's Send, and the Submit of a question card that has the keyboard.
        if let Some(question) = self.question_focus {
            self.submit_question(question, cx);
            return false;
        }
        if !self.can_send(cx) {
            return false;
        }
        // Sending follows the conversation to its end again.
        self.list.set_follow_mode(FollowMode::Tail);
        self.attach = None;
        self.model.update(cx, |model, cx| model.send(cx))
    }

    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        let info = self.info(cx);
        if self.gate(info.as_ref(), cx).running {
            self.model.update(cx, |model, cx| model.interrupt(cx));
        }
    }

    /// Answers `approval` with `option` through this window's connection.
    pub fn answer(&mut self, approval: ApprovalId, option: String, cx: &mut Context<Self>) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.respond_approval(approval, option, cx));
        let model = self.model.clone();
        cx.spawn(async move |_, cx| {
            if let Err(error) = task.await {
                model.update(cx, |model, cx| {
                    model.set_error(format!("Could not answer: {error}"), cx);
                });
            }
        })
        .detach();
    }

    // ---- vim -------------------------------------------------------------------------------

    /// The composer's Vim mode, `None` while `editor.vim_mode` is off.
    pub fn vim_mode(&self, cx: &App) -> Option<super::vim::Mode> {
        VimConfig::of(&self.settings)
            .enabled
            .then(|| self.model.read(cx).vim().mode())
    }

    /// The shape the composer's caret is drawn in under Vim mode: a block in Normal and Visual,
    /// a bar in Insert; `None` while `editor.vim_mode` is off.
    pub fn vim_caret(&self, cx: &App) -> Option<CaretShape> {
        self.vim_mode(cx).map(CaretShape::of)
    }

    /// Gives the key to Vim's modal layer. In Insert mode the composer types for itself (only
    /// Escape is Vim's); Normal and Visual keys are commands. `true` when the key is used up.
    fn on_vim_key(
        &mut self,
        event: &KeyDownEvent,
        config: VimConfig,
        cx: &mut Context<Self>,
    ) -> bool {
        let options = config.options(false, true);
        let outcome = self
            .model
            .update(cx, |model, cx| model.vim_key(event, options, cx));
        if outcome == super::vim::Outcome::Handled {
            return true;
        }
        let mods = &event.keystroke.modifiers;
        let normal = self.model.read(cx).vim().mode() != super::vim::Mode::Insert;
        if normal && event.keystroke.key == "enter" && !mods.platform {
            // Enter is not a newline in Normal mode: it sends when it would send in Insert mode.
            if self.send_key().return_sends(mods.shift, mods.alt) {
                self.send(cx);
            }
            return true;
        }
        false
    }

    // ---- keys ------------------------------------------------------------------------------

    /// The send key now in force (`chat.send_key`), read live.
    pub(super) fn send_key(&self) -> SendKey {
        self.settings.get::<ChatSettings>().send_key
    }

    fn edit(&mut self, change: impl FnOnce(&mut TextInput), cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| model.edit(change, cx));
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let mods = &keystroke.modifiers;
        let key = keystroke.key.as_str();
        let plain = !mods.platform && !mods.control && !mods.alt && !mods.shift && !mods.function;

        // A question card with the keyboard takes it, digits included: 1–4 never answer a
        // question, and never leave one that is being typed into.
        if self.on_question_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        if self.question_focus.is_some() {
            // Its field's text arrives through the platform's text input, not as a key.
            return;
        }
        // The attach form's field takes the typing while a form is open.
        if self.on_attach_key(event, cx) {
            cx.stop_propagation();
            return;
        }
        // Tab turns the mention at the caret (`@file src/lib.rs`) into what it offers.
        if plain && key == "tab" && self.accept_mention(cx) {
            cx.stop_propagation();
            return;
        }

        // 1–4 answer the oldest request while the composer is empty.
        if plain
            && let Some(number) = key.parse::<usize>().ok().filter(|n| (1..=4).contains(n))
            && self.model.read(cx).input().is_empty()
            && let Some(approval) = self.pending.first().copied()
        {
            let option = self
                .model
                .read(cx)
                .transcript()
                .approval(approval)
                .and_then(|a| a.options.get(number - 1))
                .map(|o| o.id.clone());
            if let Some(option) = option {
                self.answer(approval, option, cx);
                cx.stop_propagation();
                return;
            }
        }

        // Vim's modal layer: Normal and Visual keys are commands; Insert is typing as ever.
        let vim = VimConfig::of(&self.settings);
        if vim.enabled && self.on_vim_key(event, vim, cx) {
            cx.stop_propagation();
            return;
        }

        // ⌘V looks for files and images on the clipboard (R-1) before it types the text.
        if mods.platform && key == "v" {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        // Return sends or breaks the line (`chat.send_key`); ⌘Return is the Send action. A
        // Return that commits an input method's marked text never sends.
        if key == "enter" && !mods.platform && !mods.control {
            if self.model.read(cx).input().marked().is_some() {
                self.edit(TextInput::unmark, cx);
            } else if self.send_key().return_sends(mods.shift, mods.alt) {
                self.send(cx);
            } else {
                self.edit(|input| input.insert("\n"), cx);
            }
            cx.stop_propagation();
            return;
        }
        // Backspace in an empty composer takes the last attachment off.
        if key == "backspace"
            && !mods.platform
            && !mods.control
            && !mods.alt
            && self.backspace_removes_chip(cx)
        {
            cx.stop_propagation();
            return;
        }
        // Vim's Normal and Visual modes use no key Vim left alone, except ⌘ shortcuts.
        if vim.enabled
            && !mods.platform
            && self.model.read(cx).vim().mode() != super::vim::Mode::Insert
        {
            return;
        }
        // The rest is editing the text (arrows, Backspace, ⌘A/C/X/Z, ⌥ and ⌘ motions, Shift
        // selection). Typed text is no key press: it reaches the composer through the
        // platform's text input (`composer_field`), as do input methods and dictation.
        let used = self.model.update(cx, |model, cx| {
            model.edit_field(vim.enabled, cx, |field, cx| edit_keys(field, event, cx))
        });
        if used {
            cx.stop_propagation();
        }
    }

    /// How the composer's element reaches the draft: the platform's text input and the mouse.
    fn composer_field(&self, cx: &mut Context<Self>) -> FieldRef {
        FieldRef::new(
            cx.weak_entity(),
            self.focus_handle.clone(),
            |this, cx, f| {
                let vim = VimConfig::of(&this.settings).enabled;
                this.model.update(cx, |model, cx| {
                    model.edit_field(vim, cx, |field, _| f(field))
                });
            },
        )
    }

    // ---- rendering -------------------------------------------------------------------------

    fn render_header(
        &self,
        info: Option<&SessionInfo>,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let Some(info) = info else {
            return div().into_any_element();
        };
        if info.chat.as_ref().is_some_and(|chat| chat.is_assistant()) {
            // The Assistant page's own header row says who runs and in what state (dot, scope,
            // backend·model chip); this one only has to say that approvals wait.
            let pending = self.pending.len();
            if pending == 0 {
                return div().into_any_element();
            }
            return div()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .h(px(tokens.pane_header))
                .px(px(tokens.spacing * 3.0))
                .border_b_1()
                .border_color(theme.border)
                .child(div().min_w_0().truncate().text_color(theme.warning).child(
                    SharedString::from(format!(
                        "● {pending} approval{} waiting",
                        if pending == 1 { "" } else { "s" }
                    )),
                ))
                .child(probe(&self.regions, Region::Chat("header")))
                .into_any_element();
        }
        let presentation = self.sessions.read(cx).presentation(info);
        let chat = info.chat.as_ref();
        let detail = chat.map(|chat| {
            if is_gateway_chat(&chat.agent) {
                format!(
                    "{} · {}",
                    chat.provider_id.as_deref().unwrap_or("provider"),
                    chat.model
                )
            } else if is_harness_chat(&chat.agent) {
                // The harness decides what asks and what is sandboxed: no Hutan policy to show.
                format!("{} · {}", harness_name(&chat.agent), chat.model)
            } else {
                format!(
                    "{} · {} · approvals {} · sandbox {}",
                    chat.agent, chat.model, chat.approval_policy, chat.sandbox
                )
            }
        });
        let pending = self.pending.len();
        let session = self.session;
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .h(px(tokens.pane_header))
            .px(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .child(div().text_color(theme.muted).child(kind_icon(info.kind)))
            .child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(SharedString::from(info.title.clone())),
            )
            .child(state_dot(theme, &presentation))
            .child(
                div()
                    .text_color(tone_color(theme, presentation.tone))
                    .child(SharedString::from(presentation.label.clone())),
            )
            .children(detail.map(|text| {
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(text))
            }))
            .when(
                chat.is_some_and(|chat| {
                    is_gateway_chat(&chat.agent) || is_harness_chat(&chat.agent)
                }),
                |s| {
                    s.child(
                        button(
                            "chat-switch-model",
                            "Switch model…",
                            ButtonKind::Secondary,
                            chat.is_some_and(|chat| !chat.turn_active),
                            look,
                        )
                        .when(chat.is_some_and(|chat| !chat.turn_active), |b| {
                            b.on_click(move |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(actions::SwitchChatModelById(session.to_string())),
                                    cx,
                                );
                            })
                        })
                        .child(probe(&self.regions, Region::Chat("switch-model"))),
                    )
                },
            )
            .when(pending > 0, |s| {
                s.child(
                    div()
                        .flex_none()
                        .text_color(theme.warning)
                        .child(SharedString::from(format!(
                            "● {pending} approval{} waiting",
                            if pending == 1 { "" } else { "s" }
                        ))),
                )
            })
            .child(probe(&self.regions, Region::Chat("header")))
            .into_any_element()
    }

    pub(super) fn provider_error_action(
        &self,
        message: &str,
        look: &Look,
        cx: &App,
    ) -> Option<AnyElement> {
        let info = self.sessions.read(cx).session(self.session)?;
        let chat = info
            .chat
            .as_ref()
            .filter(|chat| is_gateway_chat(&chat.agent))?;
        let session = self.session;
        if message.contains("HTTP 401")
            || message.contains("HTTP 403")
            || message.contains("no API key")
        {
            let id = chat.provider_id.clone()?;
            Some(
                button(
                    "chat-fix-key",
                    "Set key…",
                    ButtonKind::Secondary,
                    true,
                    look,
                )
                .child(probe(&self.regions, Region::Chat("fix-key")))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(actions::SetProviderKey(id.clone())), cx)
                })
                .into_any_element(),
            )
        } else if message.contains("not found:") {
            Some(
                button(
                    "chat-fix-model",
                    "Pick another model…",
                    ButtonKind::Secondary,
                    !chat.turn_active,
                    look,
                )
                .child(probe(&self.regions, Region::Chat("fix-model")))
                .when(!chat.turn_active, |b| {
                    b.on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(actions::SwitchChatModelById(session.to_string())),
                            cx,
                        );
                    })
                })
                .into_any_element(),
            )
        } else {
            None
        }
    }

    fn render_banner(&self, look: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let model = self.model.read(cx);
        let (text, color, dismissable) = match (model.error(), model.status()) {
            (Some(error), _) => (error.to_string(), theme.danger, true),
            (None, LoadStatus::Failed(error)) => (
                format!("The conversation could not be loaded: {error}"),
                theme.warning,
                false,
            ),
            _ => return None,
        };
        let hover = theme.surface_hover;
        let recovery = self.provider_error_action(&text, look, cx);
        Some(
            div()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .text_color(color)
                .child(div().flex_1().min_w_0().child(SharedString::from(text)))
                .children(recovery)
                .when(dismissable, |s| {
                    s.child(
                        div()
                            .id("chat-error-dismiss")
                            .px(px(tokens.spacing))
                            .rounded(px(tokens.radius - 2.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, |model, cx| model.clear_error(cx));
                            }))
                            .child("×"),
                    )
                })
                .child(probe(&self.regions, Region::Chat("banner")))
                .into_any_element(),
        )
    }

    /// What the last daemon restart did to this conversation, until the user dismisses it or
    /// goes on: the chip, one sentence, and "Retry last message" when the turn was cut off.
    fn render_recovery(
        &self,
        info: Option<&SessionInfo>,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let info = info?;
        let chip = hutan_client::recovery::chip(info)?;
        let recovery = info.recovery.as_ref()?;
        let (theme, tokens) = (&look.theme, &look.tokens);
        let session = self.session;
        let interrupted = matches!(
            recovery.kind,
            hutan_protocol::recovery::RecoveryKind::ChatResumed {
                last_turn: hutan_protocol::recovery::LastTurn::Interrupted,
                ..
            }
        );
        let retry =
            interrupted && info.state == SessionState::Idle && !self.model.read(cx).sending();
        let tone = match chip.tone {
            hutan_client::recovery::ChipTone::Info => theme.accent,
            hutan_client::recovery::ChipTone::Warn => theme.warning,
        };
        let chip_element = recovery_chip(
            theme,
            &chip,
            crate::workbench::view::BadgeSite::Tab,
            session,
            false,
            &self.regions,
            cx,
        );
        Some(
            div()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .child(chip_element)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(tone)
                        .child(SharedString::from(chip.detail.clone())),
                )
                .when(retry, |s| {
                    s.child(
                        button(
                            "chat-recovery-retry",
                            "Retry last message",
                            ButtonKind::Secondary,
                            true,
                            look,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_last_message(cx)))
                        .child(probe(&self.regions, Region::Chat("retry-last"))),
                    )
                })
                .child(
                    button(
                        "chat-recovery-dismiss",
                        "Dismiss",
                        ButtonKind::Ghost,
                        true,
                        look,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let ack = this
                            .sessions
                            .update(cx, |store, cx| store.ack_recovery(Some(this.session), cx));
                        cx.spawn(async move |_, _| {
                            // The session's next update says it; a failure leaves the chip.
                            let _ = ack.await;
                        })
                        .detach();
                    }))
                    .child(probe(&self.regions, Region::Chat("recovery-dismiss"))),
                )
                .child(probe(&self.regions, Region::Chat("recovery")))
                .into_any_element(),
        )
    }

    /// Sends the last message of the conversation again (the restart cut its turn off).
    fn retry_last_message(&mut self, cx: &mut Context<Self>) {
        let task = self
            .sessions
            .update(cx, |store, cx| store.retry_last_message(self.session, cx));
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                this.update(cx, |this, cx| {
                    this.model.update(cx, |model, cx| {
                        model.set_error(format!("Could not retry: {error}"), cx);
                    });
                })
                .ok();
            }
        })
        .detach();
    }

    /// The row above the transcript while the journal has events before the first one shown:
    /// "Load earlier" reads the page before it (scrolling to the top does the same).
    fn render_earlier(&self, look: &Look, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let model = self.model.read(cx);
        if !model.has_earlier() {
            return None;
        }
        let loading = model.loading_earlier();
        let load = button(
            "chat-load-earlier",
            if loading {
                "Loading…"
            } else {
                "Load earlier"
            },
            ButtonKind::Secondary,
            !loading,
            look,
        );
        let load = if loading {
            load
        } else {
            load.on_click(cx.listener(|this, _, _, cx| {
                this.model.update(cx, |model, cx| model.load_earlier(cx));
            }))
        };
        Some(
            div()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(tokens.spacing * 2.0))
                .px(px(tokens.spacing * 3.0))
                .py(px(tokens.spacing))
                .border_b_1()
                .border_color(theme.border)
                .text_color(theme.muted)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child("Earlier messages are not shown."),
                )
                .child(load.child(probe(&self.regions, Region::Chat("load-earlier"))))
                .child(probe(&self.regions, Region::Chat("earlier")))
                .into_any_element(),
        )
    }

    fn render_empty(&self, look: &Look, cx: &App) -> AnyElement {
        let mawas = self.supports_context(cx);
        let (mascot, text) = match self.model.read(cx).status() {
            LoadStatus::Loading => (Mascot::Thinking, "Loading the conversation…"),
            LoadStatus::Failed(_) => (Mascot::Error, "No conversation to show."),
            LoadStatus::Ready if mawas => (
                Mascot::Empty,
                "Ask Mawas. What you attach (@shell, @omp, @agent, @file, @search, or the Attach button) is sent to the model of its backend; nothing else is.",
            ),
            LoadStatus::Ready => (
                Mascot::Empty,
                "Ask the agent to do something. Commands, edits and approvals appear here.",
            ),
        };
        // Mawas offers what to do first: ask, or look at what waits for the user.
        let actions =
            (mawas && matches!(self.model.read(cx).status(), LoadStatus::Ready)).then(|| {
                let ask = button(
                    "chat-empty-ask",
                    "Ask a question",
                    ButtonKind::Primary,
                    true,
                    look,
                )
                .relative()
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(actions::OpenAssistant), cx);
                })
                .child(probe(&self.regions, Region::Chat("empty-ask")));
                let review = button(
                    "chat-empty-review",
                    "Review what needs you",
                    ButtonKind::Secondary,
                    true,
                    look,
                )
                .relative()
                .on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(actions::JumpToNextAttention), cx);
                })
                .child(probe(&self.regions, Region::Chat("empty-review")));
                div()
                    .flex()
                    .gap(px(look.tokens.spacing * 2.0))
                    .child(ask)
                    .child(review)
                    .into_any_element()
            });
        ui::empty_state(mascot, text, actions, &look.theme, &look.tokens).into_any_element()
    }

    /// The composer's text with its caret and selection (while it has the keyboard), or the
    /// placeholder; editable by the mouse and the platform's text input.
    fn input_text(&self, focused: bool, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let state = {
            let model = self.model.read(cx);
            let vim = VimConfig::of(&self.settings).enabled.then(|| model.vim());
            FieldState::of(model.input(), vim)
        };
        let placeholder = if self.supports_context(cx) {
            "Ask Mawas… (@shell @file @search to attach context)"
        } else {
            "Message the agent…"
        };
        EditableText::new("chat-composer-text", state, &look.theme)
            .placeholder(placeholder)
            .multiline(true)
            .focused(focused)
            .field(Some(self.composer_field(cx)))
            .into_any_element()
    }

    /// The one-line question about the send key (once, dismissible): the two choices and a
    /// dismiss. Either choice, or the dismiss, ends the question for good.
    fn render_send_key_offer(
        &self,
        current: SendKey,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let choices = SendKey::ALL.map(|key| {
            let (id, region) = match key {
                SendKey::CmdEnter => ("chat-send-key-cmd-enter", "send-key-cmd-enter"),
                SendKey::Enter => ("chat-send-key-enter", "send-key-enter"),
            };
            let settings = self.settings.clone();
            ui::choice(id, key.short_label(), key == current, theme, tokens)
                .relative()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Err(error) = write_send_key(&settings, key) {
                        this.model.update(cx, |model, cx| {
                            model.set_error(format!("Could not save the send key: {error}"), cx);
                        });
                    }
                    cx.notify();
                }))
                .child(probe(&self.regions, Region::Chat(region)))
        });
        let settings = self.settings.clone();
        let hover = theme.surface_hover;
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child("Which key sends a message?")
            .children(choices)
            .child(
                div()
                    .id("chat-send-key-dismiss")
                    .relative()
                    .px(px(tokens.spacing))
                    .rounded(px(tokens.radius - 2.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Err(error) = stop_asking(&settings) {
                            this.model.update(cx, |model, cx| {
                                model.set_error(format!("Could not save the setting: {error}"), cx);
                            });
                        }
                        cx.notify();
                    }))
                    .child("×")
                    .child(probe(&self.regions, Region::Chat("send-key-dismiss"))),
            )
            .into_any_element()
    }

    fn render_composer(
        &mut self,
        gate: &Gate,
        window: &mut Window,
        look: &Look,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        let focused = self.focus_handle.is_focused(window);
        // A question card's field or the Attach form takes the typing while it is open.
        let typing_here = focused
            && gate.blocked.is_none()
            && self.question_focus.is_none()
            && !matches!(
                self.attach.as_ref().map(|attaching| attaching.stage),
                Some(Stage::Form(_))
            );
        let send_key = self.send_key();
        // The one-line question about the send key: once, above the first composer that has the
        // keyboard, after the setup is done.
        if focused {
            self.send_key_armed = true;
        }
        let offer = (self.send_key_armed
            && self.settings.get::<ChatSettings>().ask_send_key
            && self
                .settings
                .get::<crate::settings::AppSettings>()
                .setup_done)
            .then(|| self.render_send_key_offer(send_key, look, cx));
        let (chips, context, sending, can_send) = {
            let model = self.model.read(cx);
            (
                model.chips().to_vec(),
                model.context().to_vec(),
                model.sending(),
                self.can_send(cx),
            )
        };
        let mention_hint = self.render_mention_hint(look, cx);
        let attach_button = self.render_attach_button(look, cx);
        let vim_pill = VimConfig::of(&self.settings).enabled.then(|| {
            let mode = self.model.read(cx).vim().mode();
            vim_ui::mode_pill(mode, theme, tokens)
                .relative()
                .child(probe(&self.regions, Region::Chat("vim-mode")))
        });
        // What the footer says in place of the send-key control.
        let plain_hint: Option<String> = if let Some(blocked) = &gate.blocked {
            Some(blocked.to_string())
        } else if gate.running {
            Some("⌘. interrupts the turn".to_owned())
        } else if self.refused_attachment(cx).is_some() {
            Some("Remove the refused attachment to send".to_owned())
        } else {
            None
        };
        let hint = match plain_hint {
            Some(text) => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .child(SharedString::from(text))
                .into_any_element(),
            None => div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .child(
                    send_key_control("chat-send-key", send_key.hint("send"), theme, tokens)
                        .on_click(|event, window, cx| {
                            window.dispatch_action(
                                Box::new(actions::ShowSendKeyMenu(event.position())),
                                cx,
                            );
                        })
                        .child(probe(&self.regions, Region::Chat("send-key"))),
                )
                .into_any_element(),
        };
        let session = self.session;
        let chip_row = div().flex().flex_wrap().gap(px(tokens.spacing)).children(
            chips.iter().enumerate().map(|(index, chip)| {
                let hover = theme.surface_hover;
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .pl(px(tokens.spacing * 2.0))
                    .pr(px(tokens.spacing))
                    .h(px(tokens.sidebar_row - 4.0))
                    .rounded(px(crate::theme::PILL_RADIUS))
                    .bg(theme.accent_soft)
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(chip.label.clone()))
                    .child(
                        div()
                            .id(SharedString::from(format!("chat-chip-remove-{index}")))
                            .px(px(tokens.spacing))
                            .rounded(px(tokens.radius - 2.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model
                                    .update(cx, |model, cx| model.remove_chip(index, cx));
                            }))
                            .child("×"),
                    )
                    .child(probe(&self.regions, Region::ChatChip(index)))
            }),
        );
        let context_row = self.render_context_row(&context, look, cx);
        let open_pastes = self.render_open_pastes(&context, look);
        let input = div()
            .id("chat-input")
            .relative()
            .max_h(px(COMPOSER_MAX_HEIGHT))
            .min_h(px(tokens.font_size * 3.2))
            .overflow_y_scroll()
            .child(self.input_text(typing_here, look, cx));

        let send = round_button("chat-send", "↑", true, can_send, SEND_SIZE, look)
            .when(can_send, |s| {
                s.on_click(cx.listener(|this, _, _, cx| {
                    this.send(cx);
                }))
            })
            .child(probe(&self.regions, Region::Chat("send")));
        let interrupt = round_button("chat-interrupt", "■", true, true, SEND_SIZE, look)
            .on_click(cx.listener(|this, _, _, cx| this.interrupt(cx)))
            .child(probe(&self.regions, Region::Chat("interrupt")));
        let restart = button("chat-restart", "Restart", ButtonKind::Secondary, true, look)
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(RestartSessionById(session.to_string())), cx);
            })
            .child(probe(&self.regions, Region::Chat("restart")));

        let card = div()
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(crate::theme::READING_WIDTH))
            .gap(px(tokens.spacing * 1.5))
            .p(px(tokens.spacing * 3.0))
            .card(theme, tokens)
            .border_color(if focused { theme.accent } else { theme.border })
            .when(!chips.is_empty(), |s| s.child(chip_row))
            .when(!context.is_empty(), |s| s.child(context_row))
            .children(open_pastes)
            .child(input)
            .children(mention_hint)
            .children(offer)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(tokens.spacing * 2.0))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(tokens.spacing * 2.0))
                            .child(attach_button)
                            .children(vim_pill)
                            .child(hint),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(tokens.spacing * 2.0))
                            .when(gate.restartable, |s| s.child(restart))
                            .when(gate.running, |s| s.child(interrupt))
                            .when(!gate.running && !gate.restartable, |s| {
                                s.child(send.opacity(if sending { 0.6 } else { 1.0 }))
                            }),
                    ),
            )
            .child(probe(&self.regions, Region::Chat("composer")));
        // The composer sits in the same 720 px reading column as the transcript.
        div()
            .flex()
            .flex_none()
            .justify_center()
            .px(px(tokens.spacing * 3.0))
            .pb(px(tokens.spacing * 3.0))
            .pt(px(tokens.spacing))
            .child(card)
            .into_any_element()
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.regions.rendered(cx.entity_id());
        self.sync_questions(cx);
        self.sync_list(cx);
        self.pending = self
            .model
            .read(cx)
            .transcript()
            .pending_approvals()
            .into_iter()
            .map(|approval| approval.id)
            .collect();
        let look = Look::of(cx);
        let info = self.info(cx);
        let gate = self.gate(info.as_ref(), cx);
        let header = self.render_header(info.as_ref(), &look, cx);
        let banner = self.render_banner(&look, cx);
        let recovery = self.render_recovery(info.as_ref(), &look, cx);
        let earlier = self.render_earlier(&look, cx);
        let attach = self.render_attach(&look, cx);
        let composer = self.render_composer(&gate, window, &look, cx);
        let empty = self.model.read(cx).transcript().is_empty();
        if !empty {
            crate::frames::content_painted();
        }
        let body = if empty {
            self.render_empty(&look, cx)
        } else {
            div()
                .size_full()
                .child(
                    list(self.list.clone(), cx.processor(Self::render_entry))
                        .size_full()
                        .into_any_element(),
                )
                .into_any_element()
        };
        let drop_overlay = self.render_drop_overlay(&look);
        let theme = look.theme;
        div()
            .id("chat")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .on_action(cx.listener(|this, _: &actions::chat::SendMessage, _, cx| {
                this.send(cx);
            }))
            .on_action(
                cx.listener(|this, _: &actions::chat::InterruptTurn, _, cx| {
                    this.interrupt(cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    // The workbench re-focuses itself on this very mouse down; focus the chat
                    // once the event has been handled.
                    cx.defer_in(window, |this, window, cx| {
                        window.focus(&this.focus_handle, cx);
                    });
                    let _ = this;
                }),
            )
            .group(super::rich_view::DROP_GROUP)
            .on_drop::<gpui::ExternalPaths>(cx.listener(
                |this, paths: &gpui::ExternalPaths, window, cx| {
                    // Files dropped anywhere on the chat attach to the next message.
                    this.attach_paths(paths.paths(), cx);
                    window.focus(&this.focus_handle, cx);
                },
            ))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.bg)
            .child(header)
            .children(recovery)
            .children(banner)
            .children(earlier)
            .child(div().relative().flex_1().min_h_0().child(body))
            .children(attach)
            .child(composer)
            .child(drop_overlay)
    }
}
