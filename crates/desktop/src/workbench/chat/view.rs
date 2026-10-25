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
use super::widgets::{ButtonKind, context_text, context_tooltip};
use crate::sessions::SessionsStore;
use crate::settings::{ChatSettings, SendKey};
use crate::workbench::actions::{self, RestartSessionById};
use crate::workbench::harnesses::harness_name;
use crate::workbench::panels::{recovery_chip, tone_color};
use crate::workbench::render::probe;
use crate::workbench::send_key::{send_key_control, stop_asking, write_send_key};
use crate::workbench::text_field::{EditableText, FieldRef, FieldState, edit_keys};
use crate::workbench::ui::{self, Button, IconButton, Mascot, StyledExt as _};
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
    /// The queue's chips as drawn last, and those that left (`queue_view.rs`).
    pub(super) queue_seen: std::cell::RefCell<super::queue_view::QueueSeen>,
    /// Approvals seen waiting, for the fold of the answered card (`cards.rs`).
    pub(super) approvals_seen: std::cell::RefCell<super::cards::ApprovalsSeen>,
    focus_handle: FocusHandle,
    pub(super) list: ListState,
    seen_epoch: u64,
    /// `Transcript::prepended` as of the last sync.
    synced_front: u64,
    synced_len: usize,
    synced_rev: u64,
    /// The end of the transcript as the reader last saw it (at the bottom): what "new below"
    /// counts from.
    seen_end: TailMark,
    /// Things the user opened or closed, against each card's default: `(entry id, part)`.
    pub(super) toggled: HashSet<(u64, u32)>,
    /// Parsed markdown by entry id, with the entry revision it was parsed at.
    pub(super) parsed: HashMap<u64, (u64, Rc<Vec<Block>>)>,
    /// Cards built and markdown parsed so far (tests: a frame builds what is in view and parses
    /// each message revision once).
    pub(super) entries_built: usize,
    /// Draws a failed footer's `retry-after` countdown again in a second.
    pub(super) retry_tick: Option<gpui::Task<()>>,
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

/// A model as the chat header says it: the catalog's display name when there is one, else the
/// last segment of its id (`gitlab-duo/duo-chat-opus-4-6` reads `duo-chat-opus-4-6`).
pub(super) fn model_label(id: &str, display_name: Option<&str>) -> String {
    if let Some(name) = display_name.map(str::trim).filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    match id.rsplit('/').next() {
        Some(last) if !last.is_empty() => last.to_owned(),
        _ => id.to_owned(),
    }
}

/// The end of a transcript at one moment: how many entries it had (those put in front by "Load
/// earlier" do not count) and the revision of the last one, which grows while a reply streams.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TailMark {
    end: u64,
    last_rev: u64,
}

impl TailMark {
    fn of(transcript: &hutan_chat_view::Transcript) -> Self {
        Self {
            end: (transcript.len() as u64).saturating_sub(transcript.prepended()),
            last_rev: transcript.entries().last().map_or(0, |entry| entry.rev),
        }
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
        let checkpoints = sessions.read(cx).checkpoints().clone();
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe(&sessions, |_, _, cx| cx.notify()),
            // A turn's checkpoint arrived, went or was restored: its footer says so.
            cx.observe(&checkpoints, |this, _, cx| this.remeasure_footers(cx)),
        ];
        Self {
            session,
            model,
            sessions,
            settings,
            regions,
            queue_seen: std::cell::RefCell::default(),
            approvals_seen: std::cell::RefCell::default(),
            focus_handle: cx.focus_handle(),
            list,
            seen_epoch: 0,
            synced_front: 0,
            synced_len: 0,
            synced_rev: 0,
            seen_end: TailMark::default(),
            toggled: HashSet::new(),
            parsed: HashMap::new(),
            entries_built: 0,
            retry_tick: None,
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

    /// Whether the reader sees the end of the transcript: the list follows it, or is scrolled
    /// all the way down (the list takes up following again on the layout after it got there;
    /// this does not wait for it).
    fn at_bottom(&self) -> bool {
        if self.list.is_following_tail() {
            return true;
        }
        let end = self.list.max_offset_for_scrollbar().y;
        let at = -self.list.scroll_px_offset_for_scrollbar().y;
        at + px(1.0) >= end
    }

    /// What arrived below the reader's place while they read further up, as the pill says it
    /// (`3 new ↓`, `New output ↓` while the last entry only grew); `None` at the bottom and
    /// while nothing changed.
    pub fn new_below(&self, cx: &App) -> Option<String> {
        if self.at_bottom() {
            return None;
        }
        let now = TailMark::of(self.model.read(cx).transcript());
        let added = now.end.saturating_sub(self.seen_end.end);
        if added > 0 {
            Some(format!("{added} new ↓"))
        } else if now.end == self.seen_end.end && now.last_rev != self.seen_end.last_rev {
            Some("New output ↓".to_owned())
        } else {
            None
        }
    }

    /// The pill's click: back to the end of the conversation, following it again.
    pub fn jump_to_latest(&mut self, cx: &mut Context<Self>) {
        self.list.set_follow_mode(FollowMode::Tail);
        self.seen_end = TailMark::of(self.model.read(cx).transcript());
        cx.notify();
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

    /// Measures the turn footers again: what they say of the turn's files changed.
    fn remeasure_footers(&mut self, cx: &mut Context<Self>) {
        let footers: Vec<usize> = self
            .model
            .read(cx)
            .transcript()
            .entries()
            .iter()
            .enumerate()
            .take(self.synced_len)
            .filter(|(_, entry)| matches!(entry.body, hutan_chat_view::Body::Footer(_)))
            .map(|(at, _)| at)
            .collect();
        for at in footers {
            self.list.splice(at..at + 1, 1);
        }
        cx.notify();
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

    /// Whether the send key would queue the draft now: a turn runs, the daemon keeps a queue,
    /// and the draft has text and no attachments (those cannot wait in a queue).
    pub fn can_queue(&self, cx: &App) -> bool {
        let info = self.info(cx);
        let gate = self.gate(info.as_ref(), cx);
        let model = self.model.read(cx);
        gate.blocked.is_none()
            && gate.running
            && model.supports_queue()
            && !model.sending()
            && !model.draft_message().trim().is_empty()
            && !model.has_context()
    }

    /// Whether Send now steers the running turn (`ChatQueue::steer`); `false` means it
    /// interrupts the turn first.
    pub fn send_now_steers(&self, cx: &App) -> bool {
        self.model.read(cx).queue().is_some_and(|queue| queue.steer)
    }

    /// The label of Send now, on the composer's button and on the queue's chips: it only
    /// says "Interrupt & send" when a turn runs that has to be interrupted for it. With no turn
    /// running (after Stop, while the queue holds) the message simply goes.
    pub fn send_now_label(&self, cx: &App) -> &'static str {
        let running = self.gate(self.info(cx).as_ref(), cx).running;
        if self.send_now_steers(cx) || !running {
            "Send now"
        } else {
            "Interrupt & send"
        }
    }

    /// The text of the header's context chip (`context 61 % · 122k / 200k`); `None` while the
    /// agent has reported no context use, when the header shows no chip.
    pub fn context_meter(&self, cx: &App) -> Option<String> {
        self.model
            .read(cx)
            .transcript()
            .context_use()
            .map(|context| context_text(&context))
    }

    /// The header's "who runs it" words (`OMP · Claude Haiku 5.5`) and the model's whole id when
    /// the words do not say it (for the tooltip). The model reads by the display name the desktop
    /// last learned from the harness's catalog (kept between runs: no harness is asked for it),
    /// else by the last segment of its id. `None` for a session that is no chat.
    pub fn who(&self, info: &SessionInfo, cx: &App) -> Option<(String, Option<String>)> {
        let chat = info.chat.as_ref()?;
        let known = self.sessions.read(cx).model_name(&chat.agent, &chat.model);
        let model = model_label(&chat.model, known);
        let full_model = (chat.model != model).then(|| chat.model.clone());
        let detail = if is_gateway_chat(&chat.agent) {
            format!(
                "{} · {}",
                chat.provider_id.as_deref().unwrap_or("provider"),
                model
            )
        } else if is_harness_chat(&chat.agent) || chat.safety.is_some() {
            // The harness decides what asks and what is sandboxed: no Hutan policy to show. A
            // preset's state is the safety chip's, as the harness reported it (what Hutan asked
            // for is not repeated here, where it could disagree).
            format!("{} · {}", who_runs(&harness_name(&chat.agent), chat), model)
        } else {
            format!(
                "{} · {} · approvals {} · sandbox {}",
                who_runs(&chat.agent, chat),
                model,
                chat.approval_policy,
                chat.sandbox
            )
        };
        Some((detail, full_model))
    }

    // ---- commands --------------------------------------------------------------------------

    pub fn send(&mut self, cx: &mut Context<Self>) -> bool {
        // ⌘↩ is the composer's Send, and the Submit of a question card that has the keyboard.
        if let Some(question) = self.question_focus {
            self.submit_question(question, cx);
            return false;
        }
        // A queued message open in the composer: the send key saves it.
        if self.model.read(cx).editing().is_some() {
            return self.model.update(cx, |model, cx| model.save_edit(cx));
        }
        // While a turn runs the send key queues (D1): the message goes when the turn ends.
        if self.can_queue(cx) {
            self.list.set_follow_mode(FollowMode::Tail);
            return self.model.update(cx, |model, cx| model.queue_draft(cx));
        }
        if !self.can_send(cx) {
            return false;
        }
        // Sending follows the conversation to its end again.
        self.list.set_follow_mode(FollowMode::Tail);
        self.attach = None;
        self.model.update(cx, |model, cx| model.send(cx))
    }

    /// ⌃↩ Send now: while a turn runs the draft goes without waiting (the turn takes it at its
    /// next step, or is interrupted for it); otherwise it is the ordinary Send.
    pub fn send_now(&mut self, cx: &mut Context<Self>) -> bool {
        if self.model.read(cx).editing().is_some() {
            return false;
        }
        if !self.can_queue(cx) {
            return self.send(cx);
        }
        self.list.set_follow_mode(FollowMode::Tail);
        self.model.update(cx, |model, cx| model.send_draft_now(cx))
    }

    /// Stop (⌘.): ends the turn and clears the queue; `keep_queue` keeps it waiting.
    pub fn interrupt(&mut self, keep_queue: bool, cx: &mut Context<Self>) {
        let info = self.info(cx);
        if self.gate(info.as_ref(), cx).running {
            self.model
                .update(cx, |model, cx| model.interrupt(keep_queue, cx));
        }
    }

    /// The Stop button: with messages queued it asks what becomes of them (a menu at
    /// `position`), else it stops.
    fn stop_clicked(
        &mut self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model.read(cx).queued().is_empty() {
            self.interrupt(false, cx);
        } else {
            window.dispatch_action(
                Box::new(actions::ShowStopMenu(actions::SessionMenuAt {
                    session: self.session.to_string(),
                    position,
                })),
                cx,
            );
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
        // Escape leaves a queued message being edited as it was.
        if key == "escape" && self.model.update(cx, |model, cx| model.cancel_edit(cx)) {
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
                .child(
                    // The glyph carries the warning; the words are `fg`.
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.warning)
                                .child(crate::icons::glyph(crate::icons::Icon::StateApproval)),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_color(theme.fg)
                                .child(SharedString::from(format!(
                                    "{pending} approval{} waiting",
                                    if pending == 1 { "" } else { "s" }
                                ))),
                        ),
                )
                .child(probe(&self.regions, Region::Chat("header")))
                .into_any_element();
        }
        let presentation = self.sessions.read(cx).presentation(info);
        let chat = info.chat.as_ref();
        let (detail, full_model) = self.who(info, cx).unzip();
        let full_model = full_model.flatten();
        let session = self.session;
        let context = self.model.read(cx).transcript().context_use();
        // `UX-MODE-CHIP` (protocol 0.34): the mode a harness reports, read only.
        // A chat started with a safety preset (protocol 0.51) says what the harness reported of
        // it instead, mode included: one chip, not two that could disagree.
        let contained = info.containment.as_ref();
        let mode = chat
            .and_then(|chat| safety_chip(chat, contained))
            .or_else(|| contained.map(containment_chip))
            .or_else(|| {
                chat.filter(|chat| is_harness_chat(&chat.agent))
                    .filter(|_| self.sessions.read(cx).supports_chat_mode())
                    .map(mode_chip)
            });
        // Direct and harness chats can change their model (`chat.set_model`), between turns.
        let switchable =
            chat.is_some_and(|chat| is_gateway_chat(&chat.agent) || is_harness_chat(&chat.agent));
        let between_turns = chat.is_some_and(|chat| !chat.turn_active);
        let tooltip = |text: SharedString| {
            move |_: &mut Window, cx: &mut App| {
                cx.new(|_| crate::workbench::style::TextTooltip(text.clone()))
                    .into()
            }
        };
        let (words, attention) = status_words(&presentation);
        let state = ui::state_label(
            presentation.icon(),
            words,
            tone_color(theme, presentation.tone),
            attention,
            theme,
            tokens,
        );
        // Who runs it: `OMP · opus-4-6`, muted. Where the model can change it is a ghost menu
        // (it replaces the *Switch model…* button): ▾ and a click between turns, plain words
        // while a turn runs.
        // In a narrow pane it gives way first, but keeps the harness readable (`OMP · gitl…`).
        const WHO_MIN_WIDTH: f32 = 80.0;
        let who = detail.map(|text| {
            let detail_text = SharedString::from(text.clone());
            let words = div().min_w_0().truncate().child(SharedString::from(text));
            if !switchable {
                return div()
                    .id("chat-model")
                    .min_w(px(WHO_MIN_WIDTH))
                    .text_color(theme.muted)
                    .text_size(px(tokens.secondary_font_size))
                    .child(words)
                    .when_some(full_model.clone(), |s, id| s.tooltip(tooltip(id.into())))
                    .into_any_element();
            }
            if between_turns {
                let text = detail_text.clone();
                return ui::Button::new("chat-switch-model", text, theme, tokens)
                    .compact()
                    .ghost()
                    .max_width(280.0)
                    .tooltip(match &full_model {
                        Some(id) => format!("Switch model… (now {id})"),
                        None => "Switch model…".to_owned(),
                    })
                    .child(crate::icons::glyph(crate::icons::Icon::ActionMenuDown))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(actions::SwitchChatModelById(session.to_string())),
                            cx,
                        );
                    })
                    .child(probe(&self.regions, Region::Chat("switch-model")))
                    .into_any_element();
            }
            div()
                .id("chat-switch-model")
                .relative()
                .flex()
                .min_w(px(WHO_MIN_WIDTH))
                .items_center()
                .gap(px(2.0))
                .text_color(theme.muted)
                .text_size(px(tokens.secondary_font_size))
                .child(words)
                .tooltip(tooltip(
                    match &full_model {
                        Some(id) => format!("{id}. The model can be switched between turns"),
                        None => "The model can be switched between turns".to_owned(),
                    }
                    .into(),
                ))
                .child(probe(&self.regions, Region::Chat("switch-model")))
                .into_any_element()
        });
        // A status line, not a second title (CL-CHROME, D-C5): the tab and the window title name
        // the session; this row says its state, who runs it and what is open to change, then
        // the session's menu. A waiting approval is the state's to say and the card's to ask,
        // not a count beside it. Chips are neutral; only a mode that approves everything warns.
        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing * 3.0))
            .h(px(tokens.pane_header))
            .px(px(tokens.spacing * 3.0))
            .border_b_1()
            .border_color(theme.border)
            .child(state)
            .children(who)
            .children(mode.map(|chip| {
                let tip = SharedString::from(chip.tooltip);
                let text = SharedString::from(chip.text);
                let label = if chip.warning {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(tokens.spacing))
                        .text_size(px(tokens.secondary_font_size))
                        .whitespace_nowrap()
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.warning)
                                .child(crate::icons::glyph(crate::icons::Icon::StateWarning)),
                        )
                        .child(div().text_color(theme.warning).child(text))
                } else {
                    // Plain `muted` words, not a pill that looks like a button.
                    div()
                        .text_color(theme.muted)
                        .text_size(px(tokens.secondary_font_size))
                        .whitespace_nowrap()
                        .child(text)
                };
                div()
                    .id("chat-mode")
                    .relative()
                    .flex_none()
                    .child(label)
                    .tooltip(tooltip(tip))
                    .child(probe(&self.regions, Region::Chat("mode")))
            }))
            .children(context.map(|context| {
                let tip = SharedString::from(context_tooltip(&context));
                div()
                    .id("chat-context")
                    .relative()
                    .flex_none()
                    // Plain `muted` words, whole: the percentage and the figures read together.
                    .child(
                        div()
                            .text_color(theme.muted)
                            .text_size(px(tokens.secondary_font_size))
                            .whitespace_nowrap()
                            .child(context_text(&context)),
                    )
                    .tooltip(tooltip(tip))
                    .child(probe(&self.regions, Region::Chat("context")))
            }))
            .child(div().flex_1())
            .child(
                IconButton::new(
                    "chat-menu",
                    crate::icons::Icon::ActionMore,
                    "More",
                    theme,
                    tokens,
                )
                .on_click(move |event, window, cx| {
                    window.dispatch_action(
                        Box::new(actions::ShowSessionMenu(actions::SessionMenuAt {
                            session: session.to_string(),
                            position: event.position(),
                        })),
                        cx,
                    );
                })
                .child(probe(&self.regions, Region::Chat("menu"))),
            )
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
                Button::new("chat-fix-key", "Set key…", &look.theme, &look.tokens)
                    .child(probe(&self.regions, Region::Chat("fix-key")))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(actions::SetProviderKey(id.clone())), cx)
                    })
                    .into_any_element(),
            )
        } else if message.contains("not found:") {
            Some(
                Button::new(
                    "chat-fix-model",
                    "Pick another model…",
                    &look.theme,
                    &look.tokens,
                )
                .disabled(chat.turn_active)
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
                        IconButton::new(
                            "chat-error-dismiss",
                            crate::icons::Icon::ActionClose,
                            "Dismiss",
                            theme,
                            tokens,
                        )
                        .compact()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.model.update(cx, |model, cx| model.clear_error(cx));
                        })),
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
                        Button::new(
                            "chat-recovery-retry",
                            "Retry last message",
                            &look.theme,
                            &look.tokens,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.retry_last_message(cx)))
                        .child(probe(&self.regions, Region::Chat("retry-last"))),
                    )
                })
                .child(
                    Button::new(
                        "chat-recovery-dismiss",
                        "Dismiss",
                        &look.theme,
                        &look.tokens,
                    )
                    .ghost()
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
    pub(super) fn retry_last_message(&mut self, cx: &mut Context<Self>) {
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
        let load = Button::new(
            "chat-load-earlier",
            if loading {
                "Loading…"
            } else {
                "Load earlier"
            },
            &look.theme,
            &look.tokens,
        )
        .disabled(loading);
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

    /// The pill over the bottom of the transcript while the reader is further up and output
    /// arrived: one click goes to the end and follows it again.
    fn render_jump(&self, text: String, look: &Look, cx: &mut Context<Self>) -> AnyElement {
        let (theme, tokens) = (&look.theme, &look.tokens);
        div()
            .absolute()
            .bottom(px(tokens.spacing * 3.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                Button::new("chat-jump-latest", SharedString::from(text), theme, tokens)
                    .compact()
                    .on_click(cx.listener(|this, _, _, cx| this.jump_to_latest(cx)))
                    .child(probe(&self.regions, Region::Chat("jump-latest"))),
            )
            .into_any_element()
    }

    /// The empty transcript: the mascot, one sentence, no button and, for Mawas, one muted
    /// line on what is sent to the model. Mawas's composer below holds the keyboard
    /// (placeholder `Ask Mawas…`), so asking needs no second way in; looking at what waits for
    /// the user is Today's *Review requests*, ⌘⇧J and the Agents page.
    fn render_empty(&self, look: &Look, cx: &App) -> AnyElement {
        let mawas = self.supports_context(cx);
        let (mascot, text) = match self.model.read(cx).status() {
            LoadStatus::Loading => (Mascot::Thinking, "Loading the conversation…"),
            LoadStatus::Failed(_) => (Mascot::Error, "No conversation to show."),
            LoadStatus::Ready if mawas => (
                Mascot::Empty,
                "Ask Mawas anything. Attach context with @ or\u{a0}+.",
            ),
            LoadStatus::Ready => (
                Mascot::Empty,
                "Ask the agent to do something. Commands, edits and approvals appear here.",
            ),
        };
        let empty = ui::EmptyState::new(text).mascot(mascot);
        // What leaves the machine, said once and quietly: the composer's chips show what is
        // attached.
        let empty = if mawas && matches!(self.model.read(cx).status(), LoadStatus::Ready) {
            empty.hint("Only what you attach is sent to the model of its backend.")
        } else {
            empty
        };
        empty.render(&look.theme, &look.tokens).into_any_element()
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
            "Ask Mawas…"
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
        div()
            .flex()
            .items_center()
            .gap(px(tokens.spacing * 2.0))
            .text_size(px(tokens.secondary_font_size))
            .text_color(theme.muted)
            .child("Which key sends a message?")
            .children(choices)
            .child(
                IconButton::new(
                    "chat-send-key-dismiss",
                    crate::icons::Icon::ActionClose,
                    "Dismiss",
                    theme,
                    tokens,
                )
                .compact()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Err(error) = stop_asking(&settings) {
                        this.model.update(cx, |model, cx| {
                            model.set_error(format!("Could not save the setting: {error}"), cx);
                        });
                    }
                    cx.notify();
                }))
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
        let (editing, stopping, queues) = {
            let model = self.model.read(cx);
            (
                model.editing().is_some(),
                model.stopping(),
                model.supports_queue(),
            )
        };
        let can_queue = self.can_queue(cx);
        let steers = self.send_now_steers(cx);
        let send_now_label = self.send_now_label(cx);
        let queue_strip = self.render_queue(look, cx);
        let mention_hint = self.render_mention_hint(look, cx);
        let attach_button = self.render_attach_button(look, cx);
        let vim_pill = VimConfig::of(&self.settings).enabled.then(|| {
            let mode = self.model.read(cx).vim().mode();
            vim_ui::mode_pill(mode, theme, tokens)
                .relative()
                .child(probe(&self.regions, Region::Chat("vim-mode")))
        });
        // What the footer says in place of the send-key control.
        let send_symbol = match send_key {
            SendKey::CmdEnter => "⌘↩",
            SendKey::Enter => "↩",
        };
        let plain_hint: Option<String> = if let Some(blocked) = &gate.blocked {
            Some(blocked.to_string())
        } else if editing {
            Some(format!(
                "Editing a queued message · {send_symbol} saves · esc keeps it as it was"
            ))
        } else if gate.running && stopping {
            Some("Stopping…".to_owned())
        } else if gate.running && queues {
            Some(format!(
                "{send_symbol} queues · ⌃↩ {} · ⌘. stops",
                if steers {
                    "sends now"
                } else {
                    "interrupts and sends"
                }
            ))
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
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .pl(px(tokens.spacing * 2.0))
                    .pr(px(tokens.spacing))
                    .h(px(tokens.sidebar_row - 4.0))
                    .rounded(px(crate::theme::pill_radius()))
                    .bg(theme.accent_soft)
                    .font_family(look.mono.clone())
                    .text_size(px(tokens.secondary_font_size))
                    .child(SharedString::from(chip.label.clone()))
                    .child(
                        IconButton::new(
                            SharedString::from(format!("chat-chip-remove-{index}")),
                            crate::icons::Icon::ActionClose,
                            "Remove",
                            theme,
                            tokens,
                        )
                        .compact()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model
                                .update(cx, |model, cx| model.remove_chip(index, cx));
                        })),
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

        let send = IconButton::new(
            "chat-send",
            crate::icons::Icon::ActionForward,
            "Send",
            &look.theme,
            &look.tokens,
        )
        .round()
        .side(SEND_SIZE)
        .primary()
        .disabled(!can_send)
        .when(can_send, |s| {
            s.on_click(cx.listener(|this, _, _, cx| {
                this.send(cx);
            }))
        })
        .child(probe(&self.regions, Region::Chat("send")));
        let interrupt = IconButton::new(
            "chat-interrupt",
            crate::icons::Icon::ActionInterrupt,
            "Stop",
            &look.theme,
            &look.tokens,
        )
        .round()
        .side(SEND_SIZE)
        .primary()
        .disabled(stopping)
        .when(!stopping, |s| {
            s.on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                this.stop_clicked(event.position(), window, cx);
            }))
        })
        .child(probe(&self.regions, Region::Chat("interrupt")));
        // While a turn runs: the send key's button queues, Send now does not wait (it says
        // when it interrupts the turn).
        let queue_button = IconButton::new(
            "chat-queue",
            crate::icons::Icon::ActionForward,
            "Queue",
            &look.theme,
            &look.tokens,
        )
        .kind(ButtonKind::Secondary)
        .round()
        .side(SEND_SIZE)
        .disabled(!can_queue)
        .when(can_queue, |s| {
            s.on_click(cx.listener(|this, _, _, cx| {
                this.send(cx);
            }))
        })
        .child(probe(&self.regions, Region::Chat("queue")));
        let send_now = Button::new("chat-send-now", send_now_label, &look.theme, &look.tokens)
            .ghost()
            .disabled(!can_queue)
            .when(can_queue, |s| {
                s.on_click(cx.listener(|this, _, _, cx| {
                    this.send_now(cx);
                }))
            })
            .child(probe(&self.regions, Region::Chat("send-now")));
        let save = Button::new("chat-edit-save", "Save", &look.theme, &look.tokens)
            .primary()
            .on_click(cx.listener(|this, _, _, cx| {
                this.send(cx);
            }))
            .child(probe(&self.regions, Region::Chat("edit-save")));
        let cancel = Button::new("chat-edit-cancel", "Cancel", &look.theme, &look.tokens)
            .ghost()
            .on_click(cx.listener(|this, _, _, cx| {
                this.model.update(cx, |model, cx| model.cancel_edit(cx));
            }))
            .child(probe(&self.regions, Region::Chat("edit-cancel")));
        let restart = Button::new("chat-restart", "Restart", &look.theme, &look.tokens)
            .on_click(move |_, window, cx| {
                window.dispatch_action(Box::new(RestartSessionById(session.to_string())), cx);
            })
            .child(probe(&self.regions, Region::Chat("restart")));

        let card = div()
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(crate::theme::reading_width()))
            .gap(px(tokens.spacing * 1.5))
            .p(px(tokens.spacing * 3.0))
            // A card with the inputs' hairline; no accent, focused or not (the caret shows it).
            .card(theme, tokens)
            .border_1()
            .border_color(theme.border)
            .children(queue_strip)
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
                            .items_center()
                            .gap(px(tokens.spacing * 2.0))
                            .when(gate.restartable, |s| s.child(restart))
                            .when(editing, |s| s.child(cancel).child(save))
                            .when(!editing && gate.running && queues, |s| {
                                s.child(send_now).child(queue_button)
                            })
                            .when(gate.running, |s| s.child(interrupt))
                            .when(!editing && !gate.running && !gate.restartable, |s| {
                                s.child(send.loading(sending))
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
        // What the reader has seen of the end is what was there while they were at the bottom;
        // a transcript that started over (a daemon restart) starts the count again.
        let now = TailMark::of(self.model.read(cx).transcript());
        if self.at_bottom() || now.end < self.seen_end.end {
            self.seen_end = now;
        }
        let jump = self
            .new_below(cx)
            .map(|text| self.render_jump(text, &look, cx));
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
            .key_context(crate::workbench::vim_ui::vim_key_context(
                KEY_CONTEXT,
                self.vim_mode(cx),
            ))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .on_action(cx.listener(|this, _: &actions::chat::SendMessage, _, cx| {
                this.send(cx);
            }))
            .on_action(
                cx.listener(|this, _: &actions::chat::InterruptTurn, _, cx| {
                    this.interrupt(false, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &actions::chat::SendNow, _, cx| {
                this.send_now(cx);
            }))
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
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(body)
                    .children(jump),
            )
            .children(attach)
            .child(composer)
            .child(drop_overlay)
    }
}

/// The words of the status line's state, and whether they are drawn in `fg` (the glyph alone
/// in the state's color). What waits for a decision is said as such — `Approval`, `Question`,
/// `Plan to approve` — with the warning on the glyph; finished work and a failure say so
/// (`Ready for review · turn finished`, `Failed · exit 1`) in the state's color; any other state
/// is its word (`idle`, `working`).
pub fn status_words(presentation: &crate::sessions::attention::Presentation) -> (String, bool) {
    let Some(word) = presentation.state_word() else {
        return (presentation.label.clone(), false);
    };
    let words = if presentation.detail.is_empty() {
        word.to_owned()
    } else {
        format!("{word} · {}", presentation.detail)
    };
    (words, presentation.is_request())
}

/// The chat header's mode chip (`UX-MODE-CHIP`, protocol 0.34).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChip {
    pub text: String,
    pub tooltip: String,
    /// The mode lets the agent act without asking: drawn as a warning.
    pub warning: bool,
}

/// The agent as the header names it: `Claude Code (Work)` when it runs as a managed account
/// (protocol 0.50), else just its name.
pub fn who_runs(name: &str, chat: &hutan_protocol::chat::ChatInfo) -> String {
    match &chat.account {
        Some(account) => format!("{name} ({})", account.label),
        None => name.to_owned(),
    }
}

/// What the header says about the mode a harness runs in: `Mode: Plan`, a warning for a mode
/// that approves everything, `Mode unknown` when the harness reports none. Hutan never changes
/// the mode (user decision D4), so the chip has no action.
pub fn mode_chip(chat: &hutan_protocol::chat::ChatInfo) -> ModeChip {
    match &chat.mode {
        Some(mode) if mode.approves_everything => ModeChip {
            text: format!("Mode: {} · approves everything", mode.label()),
            tooltip: format!(
                "The agent runs in its “{}” mode ({}): it acts without asking for approval. \
                 Hutan shows the mode the agent reports and never changes it; change it in the \
                 agent's own settings.",
                mode.label(),
                mode.id
            ),
            warning: true,
        },
        Some(mode) => ModeChip {
            text: format!("Mode: {}", mode.label()),
            tooltip: format!(
                "The mode the agent reports it runs in ({}). Hutan shows it and never changes it.",
                mode.id
            ),
            warning: false,
        },
        None => ModeChip {
            text: "Mode unknown".to_owned(),
            tooltip: "This harness does not report its mode. Its own settings decide what it may \
                      do without asking."
                .to_owned(),
            warning: false,
        },
    }
}

/// What the header says of a chat's safety preset (protocol 0.51, ADR 0021 decision 6): the
/// state **the harness reported**, never only what Hutan asked for, with the holes named.
/// `None` for a chat created without a preset.
///
/// - confirmed: `Contained · workspace-write · network off` (Codex), `Contained · Accept Edits ·
///   sandbox requested` (Claude Code: its mode is read back, its Bash sandbox is not),
///   `Ask · requested` (OMP reports neither its approval mode nor its tool list);
/// - *Full access*: a warning, `Full access · no boundary`;
/// - not confirmed (the harness reported something else): a warning, `Contained · not applied`.
pub fn safety_chip(
    chat: &hutan_protocol::chat::ChatInfo,
    containment: Option<&hutan_core::Containment>,
) -> Option<ModeChip> {
    let safety = chat.safety.as_ref()?;
    let label = safety.preset.label();
    let full = safety.preset.is_full_access();
    let holes = hutan_core::safety::describe(&chat.agent, safety.preset).unwrap_or("");
    let note = safety.note.as_deref().unwrap_or("");
    let tooltip = |first: &str| {
        [
            first,
            holes,
            note,
            "Hutan shows what the harness reported and never changes it.",
        ]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
    };
    if safety.enforced_by == hutan_core::SafetyEnforcer::None && !full {
        return Some(ModeChip {
            text: format!("{label} · not applied"),
            tooltip: tooltip(&format!(
                "{label} was requested, but the agent did not confirm it."
            )),
            warning: true,
        });
    }
    if full {
        return Some(ModeChip {
            text: format!("{label} · no boundary"),
            tooltip: tooltip("The agent acts without asking and nothing limits what it can touch."),
            warning: true,
        });
    }
    let mut parts = vec![label.to_owned()];
    match chat.agent.as_str() {
        "codex" => {
            parts.push(chat.sandbox.as_str().to_owned());
            if let Some(network) = safety.network {
                parts.push(format!("network {}", if network { "on" } else { "off" }));
            }
        }
        "claude" => {
            if let Some(mode) = &chat.mode {
                parts.push(mode.label().to_owned());
            }
            if safety.preset == hutan_core::SafetyPreset::Contained {
                parts.push("sandbox requested".to_owned());
            }
        }
        _ if safety.enforced_by == hutan_core::SafetyEnforcer::Hutan => {
            // Hutan's own boundary: the chip names what it holds back and what it leaves open.
            parts.push(
                containment.map_or_else(|| "Seatbelt".to_owned(), hutan_core::Containment::summary),
            );
        }
        _ => parts.push("requested".to_owned()),
    }
    Some(ModeChip {
        text: parts.join(" · "),
        tooltip: tooltip(&format!("{label}: what the agent reported it enforces.")),
        warning: false,
    })
}

/// What a session says of the boundary Hutan put around it (protocol 0.52, ADR 0021 decision 6):
/// *writes limited to this worktree · can read your files · network off*, never a bare
/// "secure". A boundary that was asked for and is not there is a warning with the reason.
pub fn containment_chip(containment: &hutan_core::Containment) -> ModeChip {
    if !containment.applied {
        return ModeChip {
            text: containment.summary(),
            tooltip: "The session asked for Hutan's Seatbelt boundary and does not have it: it \
                      was not started uncontained."
                .to_owned(),
            warning: true,
        };
    }
    let roots = containment
        .write_roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    ModeChip {
        text: containment.summary(),
        tooltip: format!(
            "Hutan's Seatbelt boundary ({}). The process can write only in {roots} and the temp \
             folders; what it is refused fails with \"Operation not permitted\". Reads stay open \
             except credential folders and Hutan's own data.",
            containment.kind.label()
        ),
        warning: false,
    }
}

#[cfg(test)]
mod safety_chip_tests {
    use hutan_core::{SafetyEnforcer, SafetyPreset, SandboxMode};
    use hutan_protocol::chat::{ChatInfo, ChatMode, ChatSafety};

    use super::safety_chip;

    fn chat(
        agent: &str,
        preset: SafetyPreset,
        enforced_by: SafetyEnforcer,
        network: Option<bool>,
        note: Option<&str>,
    ) -> ChatInfo {
        ChatInfo {
            agent: agent.into(),
            model: "m".into(),
            approval_policy: Default::default(),
            sandbox: SandboxMode::WorkspaceWrite,
            turn_active: false,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: None,
            agent_session_id: None,
            assistant: false,
            images: None,
            mode: None,
            checkpoints: None,
            account: None,
            safety: Some(ChatSafety {
                preset,
                enforced_by,
                network,
                note: note.map(str::to_owned),
            }),
        }
    }

    #[test]
    fn a_chat_without_a_preset_has_no_safety_chip() {
        let mut info = chat(
            "codex",
            SafetyPreset::Ask,
            SafetyEnforcer::Harness,
            None,
            None,
        );
        info.safety = None;
        assert!(safety_chip(&info, None).is_none());
    }

    #[test]
    fn codex_says_its_sandbox_and_network_as_it_reported_them() {
        let chip = safety_chip(
            &chat(
                "codex",
                SafetyPreset::Contained,
                SafetyEnforcer::Harness,
                Some(false),
                None,
            ),
            None,
        )
        .unwrap();
        assert_eq!(chip.text, "Contained · workspace-write · network off");
        assert!(!chip.warning);
        assert!(chip.tooltip.contains("never changes it"));
    }

    #[test]
    fn claude_names_its_mode_and_says_the_sandbox_is_only_requested() {
        let mut info = chat(
            "claude",
            SafetyPreset::Contained,
            SafetyEnforcer::Harness,
            None,
            Some(
                "Mode confirmed. The Bash sandbox was requested through session metadata; Claude Code does not report it back.",
            ),
        );
        info.mode = Some(ChatMode::new("acceptEdits", Some("Accept Edits".into())));
        let chip = safety_chip(&info, None).unwrap();
        assert_eq!(chip.text, "Contained · Accept Edits · sandbox requested");
        assert!(chip.tooltip.contains("does not report it back"));
        assert!(!chip.warning);
    }

    #[test]
    fn omp_says_requested_because_it_reports_nothing_back() {
        let chip = safety_chip(
            &chat(
                "omp",
                SafetyPreset::Ask,
                SafetyEnforcer::Harness,
                None,
                Some(
                    "The approval mode was passed on OMP's command line; OMP does not report it back.",
                ),
            ),
            None,
        )
        .unwrap();
        assert_eq!(chip.text, "Ask · requested");
        assert!(!chip.warning);
    }

    #[test]
    fn omp_contained_names_the_boundary_and_its_holes_from_what_was_applied() {
        let boundary = hutan_core::Containment {
            kind: hutan_core::ContainKind::Seatbelt,
            network: false,
            write_roots: vec!["/work/repo".into()],
            applied: true,
            reason: None,
        };
        let chip = safety_chip(
            &chat(
                "omp",
                SafetyPreset::Contained,
                SafetyEnforcer::Hutan,
                Some(false),
                None,
            ),
            Some(&boundary),
        )
        .unwrap();
        assert_eq!(
            chip.text,
            "Contained · writes limited to this worktree · can read your files · network off"
        );
        assert!(!chip.warning);
        assert!(!chip.text.to_lowercase().contains("secure"));
        let plain = super::containment_chip(&boundary);
        assert_eq!(
            plain.text,
            "writes limited to this worktree · can read your files · network off"
        );
        assert!(
            plain.tooltip.contains("/work/repo")
                && plain.tooltip.contains("Operation not permitted")
        );
        let refused = super::containment_chip(&hutan_core::Containment {
            applied: false,
            reason: Some("sandbox-exec is missing".into()),
            ..boundary
        });
        assert!(refused.warning && refused.text.starts_with("not contained: "));
    }

    #[test]
    fn full_access_and_an_unconfirmed_preset_are_warnings() {
        let full = safety_chip(
            &chat(
                "codex",
                SafetyPreset::Full,
                SafetyEnforcer::None,
                Some(true),
                None,
            ),
            None,
        )
        .unwrap();
        assert_eq!(full.text, "Full access · no boundary");
        assert!(full.warning);
        let unapplied = safety_chip(
            &chat(
                "claude",
                SafetyPreset::Contained,
                SafetyEnforcer::None,
                None,
                Some("Not applied: Claude Code is in mode default (asked for acceptEdits)"),
            ),
            None,
        )
        .unwrap();
        assert_eq!(unapplied.text, "Contained · not applied");
        assert!(unapplied.warning);
        assert!(unapplied.tooltip.contains("is in mode default"));
    }
}

#[cfg(test)]
mod mode_chip_tests {
    use hutan_protocol::chat::{ChatInfo, ChatMode};

    use super::{mode_chip, status_words};
    use crate::sessions::attention::{Attention, Presentation, Tone};

    #[test]
    fn the_status_line_says_what_waits_as_such() {
        let p = |label: &str, detail: &str, tone, attention: Option<Attention>| Presentation {
            label: label.into(),
            detail: detail.into(),
            tone,
            filled: attention.is_some(),
            attention,
        };
        assert_eq!(
            status_words(&p("approval", "", Tone::Warning, Some(Attention::Request))),
            ("Approval".to_owned(), true)
        );
        assert_eq!(
            status_words(&p(
                "question",
                "2 questions",
                Tone::Warning,
                Some(Attention::Request)
            )),
            ("Question · 2 questions".to_owned(), true)
        );
        assert_eq!(
            status_words(&p("plan", "", Tone::Warning, Some(Attention::Request))),
            ("Plan to approve".to_owned(), true)
        );
        assert_eq!(
            status_words(&p("failed", "exit 1", Tone::Error, Some(Attention::Failed))),
            ("Failed · exit 1".to_owned(), false)
        );
        assert_eq!(
            status_words(&p(
                "review",
                "turn finished",
                Tone::Accent,
                Some(Attention::Review)
            )),
            ("Ready for review · turn finished".to_owned(), false)
        );
        assert_eq!(
            status_words(&p("idle", "OMP · opus", Tone::Muted, None)),
            ("idle".to_owned(), false)
        );
    }

    fn chat(mode: Option<ChatMode>) -> ChatInfo {
        ChatInfo {
            agent: "claude".into(),
            model: "sonnet".into(),
            approval_policy: Default::default(),
            sandbox: Default::default(),
            turn_active: false,
            pending_approvals: 0,
            pending_questions: 0,
            provider_id: None,
            agent_session_id: None,
            assistant: false,
            images: None,
            mode,
            checkpoints: None,
            account: None,
            safety: None,
        }
    }

    #[test]
    fn the_chip_names_the_reported_mode_warns_for_bypass_and_says_when_none_is_reported() {
        let plan = mode_chip(&chat(Some(ChatMode::new("plan", Some("Plan Mode".into())))));
        assert_eq!(plan.text, "Mode: Plan Mode");
        assert!(!plan.warning);
        let bypass = mode_chip(&chat(Some(ChatMode::new(
            "bypassPermissions",
            Some("Bypass Permissions".into()),
        ))));
        assert_eq!(
            bypass.text,
            "Mode: Bypass Permissions · approves everything"
        );
        assert!(bypass.warning);
        let bare = mode_chip(&chat(Some(ChatMode::new("default", None))));
        assert_eq!(bare.text, "Mode: default");
        let unknown = mode_chip(&chat(None));
        assert_eq!(unknown.text, "Mode unknown");
        assert!(unknown.tooltip.contains("does not report"));
    }
}

#[cfg(test)]
mod model_label_tests {
    use super::model_label;

    #[test]
    fn a_raw_id_reads_as_its_last_segment_and_a_catalog_name_wins() {
        assert_eq!(
            model_label("gitlab-duo/duo-chat-opus-4-6", None),
            "duo-chat-opus-4-6"
        );
        assert_eq!(
            model_label("gitlab-duo/duo-chat-opus-4-6", Some("Duo Chat Opus 4.6")),
            "Duo Chat Opus 4.6"
        );
        assert_eq!(model_label("sonnet", None), "sonnet");
        assert_eq!(model_label("sonnet", Some("  ")), "sonnet");
        assert_eq!(model_label("anthropic/", None), "anthropic/");
    }
}
