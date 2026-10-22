//! [`ChatModel`]: one chat session's transcript and composer, shared by every view of it.
//!
//! It loads the journal (`chat.history`), then follows the connection's chat events. The first
//! read is the newest [`HISTORY_DEFAULT_LIMIT`] events; [`load_earlier`](ChatModel::load_earlier)
//! pages back with `before_seq` until the journal has no more. A reconnect or a lagging
//! receiver reads the journal again and applies only what is new; a daemon restart (another
//! epoch) starts the transcript over. See `hutan_chat_view::model` for why applying an event
//! twice is harmless.

use std::collections::{HashMap, HashSet};

use gpui::{Context, SharedString, Task};
use hutan_chat_view::Transcript;
use hutan_client::{ChatUpdate, ClientError, Connection};
use hutan_core::{AgentEvent, ApprovalId, Epoch, ItemId, SessionId};
use hutan_protocol::ErrorCode;
use hutan_protocol::assistant::{AttachmentKind, AttachmentMeta, attachments_meta};
use hutan_protocol::chat::{ChatQueue, HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, QueuedMessage};
use tokio::sync::broadcast::error::RecvError;

use super::chip::{Chip, compose};
use super::context::{ContextChip, refused_by};
use super::input::TextInput;
use super::vim::{Options, Outcome, Vim};
use crate::workbench::text_field::FieldMut;

/// Pages of [`HISTORY_MAX_LIMIT`] events a reload reads at most to catch up with what happened
/// while it was not listening.
const MAX_CATCH_UP_PAGES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    Loading,
    Ready,
    /// The journal could not be read (offline, or the daemon refused).
    Failed(String),
}

#[derive(Debug)]
struct History {
    /// Oldest first.
    events: Vec<AgentEvent>,
    /// The journal has more before the first event.
    more_before: bool,
}

/// What of the journal the transcript holds, from the front.
#[derive(Debug, Clone, Copy, Default)]
struct Earlier {
    /// The oldest `seq` read: the next page is the one before it.
    first_seq: Option<u64>,
    /// The journal has events before `first_seq`.
    more: bool,
    /// A page is on its way.
    loading: bool,
    /// Bumped when the transcript starts over, so that a page that was asked for before is
    /// ignored when it arrives.
    generation: u64,
}

pub struct ChatModel {
    session: SessionId,
    connection: Connection,
    transcript: Transcript,
    status: LoadStatus,
    /// How much of the journal's front is loaded.
    earlier: Earlier,
    input: TextInput,
    /// Vim's modal state over `input` (idle unless `editor.vim_mode` is on).
    vim: Vim,
    chips: Vec<Chip>,
    /// Structured context for the next message (assistant chats): the daemon reads it.
    context: Vec<ContextChip>,
    /// What the daemon says about the attachments of each user message of the transcript.
    attachments: HashMap<ItemId, Vec<AttachmentMeta>>,
    sending: bool,
    /// The last thing that went wrong with a command (send, interrupt, answer).
    error: Option<SharedString>,
    /// What was taken out of the composer by a send that has not been answered yet.
    unsent: Option<(String, Vec<Chip>, Vec<ContextChip>)>,
    /// The chat's queue as the daemon last said it (protocol 0.36); `None` before the first
    /// read and with a daemon that has no queue.
    queue: Option<ChatQueue>,
    /// Stop was asked for and the turn has not ended yet ("Stopping…").
    stopping: bool,
    /// A queued message is open in the composer: the send key saves it.
    editing: Option<Editing>,
    _task: Task<()>,
}

/// A queued message being edited in the composer.
#[derive(Debug, Clone)]
struct Editing {
    item: u64,
    /// What the composer held before, put back when the edit ends.
    draft: String,
}

impl std::fmt::Debug for ChatModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatModel")
            .field("session", &self.session)
            .field("status", &self.status)
            .field("entries", &self.transcript.len())
            .finish_non_exhaustive()
    }
}

impl ChatModel {
    pub fn new(session: SessionId, connection: Connection, cx: &mut Context<Self>) -> Self {
        // Subscribed before the first read, so nothing between the two is lost.
        let mut updates = connection.chat_updates();
        let reader = connection.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut known: Option<Epoch> = None;
            let mut load = true;
            loop {
                if load {
                    let stop_at = this
                        .update(cx, |this, _| {
                            let seen = this.transcript.max_seq();
                            (seen > 0).then_some(seen)
                        })
                        .ok()
                        .flatten();
                    let history = fetch_history(&reader, session, stop_at).await;
                    let pending = match &history {
                        Ok(_) => reader.approvals(Some(session)).await.ok(),
                        Err(_) => None,
                    };
                    let asked = match &history {
                        Ok(_) => reader.questions(Some(session)).await.ok(),
                        Err(_) => None,
                    };
                    // The daemon keeps the queue: a reconnect reads it again.
                    let queue = match &history {
                        Ok(_) if reader.supports_chat_queue() => {
                            reader.chat_queue(session).await.ok()
                        }
                        _ => None,
                    };
                    let alive = this.update(cx, |this, cx| {
                        this.installed(history, pending, asked, cx);
                        if let Some(queue) = queue {
                            this.set_queue(queue, cx);
                        }
                    });
                    if alive.is_err() {
                        return;
                    }
                    load = false;
                }
                match updates.recv().await {
                    Ok(ChatUpdate::Event(event)) => {
                        if event.session_id != session {
                            continue;
                        }
                        let alive = this.update(cx, |this, cx| {
                            this.note_attachments(&event);
                            if event.kind.ends_turn() && this.stopping {
                                this.stopping = false;
                                cx.notify();
                            }
                            if this.transcript.apply(&event) {
                                cx.notify();
                            }
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                    Ok(ChatUpdate::Queue(queue)) => {
                        if queue.session_id != session {
                            continue;
                        }
                        let alive = this.update(cx, |this, cx| {
                            this.set_queue((*queue).clone(), cx);
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                    Ok(ChatUpdate::Resync { epoch }) => {
                        if known.is_some_and(|before| before != epoch) {
                            // The daemon restarted: its sequence numbers start over.
                            let alive = this.update(cx, |this, cx| {
                                this.transcript.reset();
                                this.earlier = Earlier {
                                    generation: this.earlier.generation + 1,
                                    ..Earlier::default()
                                };
                                cx.notify();
                            });
                            if alive.is_err() {
                                return;
                            }
                        }
                        known = Some(epoch);
                        load = true;
                    }
                    // Checkpoints are the `CheckpointStore`'s.
                    Ok(ChatUpdate::Checkpoint(_)) => {}
                    Err(RecvError::Lagged(_)) => load = true,
                    Err(RecvError::Closed) => return,
                }
            }
        });
        Self {
            session,
            connection,
            transcript: Transcript::new(),
            status: LoadStatus::Loading,
            earlier: Earlier::default(),
            input: TextInput::new(),
            vim: Vim::new(),
            chips: Vec::new(),
            context: Vec::new(),
            attachments: HashMap::new(),
            sending: false,
            error: None,
            unsent: None,
            queue: None,
            stopping: false,
            editing: None,
            _task: task,
        }
    }

    fn installed(
        &mut self,
        history: Result<History, ClientError>,
        pending: Option<Vec<hutan_protocol::chat::ApprovalInfo>>,
        asked: Option<Vec<hutan_protocol::chat::QuestionInfo>>,
        cx: &mut Context<Self>,
    ) {
        match history {
            Ok(history) => {
                if self.transcript.is_empty() {
                    // The first page of this transcript: the front of what is loaded.
                    self.earlier.first_seq = history.events.first().map(|event| event.seq);
                    self.earlier.more = history.more_before;
                }
                for event in &history.events {
                    self.note_attachments(event);
                    self.transcript.apply(event);
                }
                if let Some(pending) = pending {
                    let ids: HashSet<ApprovalId> = pending.iter().map(|a| a.id).collect();
                    self.transcript.expire_missing(&ids);
                }
                if let Some(asked) = asked {
                    let ids: HashSet<hutan_core::QuestionId> = asked.iter().map(|q| q.id).collect();
                    self.transcript.expire_missing_questions(&ids);
                }
                self.status = LoadStatus::Ready;
                // A Stop whose turn ended while this was not listening is over.
                if !self.transcript.turn_active() {
                    self.stopping = false;
                }
            }
            Err(error) => {
                // Offline: what is shown stays; a reconnect reads again.
                if self.transcript.is_empty() {
                    self.status = LoadStatus::Failed(error.to_string());
                }
            }
        }
        cx.notify();
    }

    // ---- read access -----------------------------------------------------------------------

    pub fn session(&self) -> SessionId {
        self.session
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn status(&self) -> &LoadStatus {
        &self.status
    }

    /// The journal has events older than the ones shown ("Load earlier").
    pub fn has_earlier(&self) -> bool {
        self.earlier.more
    }

    /// A page of earlier events is on its way.
    pub fn loading_earlier(&self) -> bool {
        self.earlier.loading
    }

    /// Reads the page of the journal before the oldest event shown and puts it in front of the
    /// transcript. Nothing happens when the journal has nothing more or a page is on its way.
    pub fn load_earlier(&mut self, cx: &mut Context<Self>) {
        let Some(before) = self.earlier.first_seq.filter(|_| self.earlier.more) else {
            return;
        };
        if self.earlier.loading {
            return;
        }
        self.earlier.loading = true;
        let generation = self.earlier.generation;
        let connection = self.connection.clone();
        let session = self.session;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let page = connection
                .chat_history(session, Some(before), HISTORY_DEFAULT_LIMIT)
                .await;
            this.update(cx, |this, cx| {
                if this.earlier.generation != generation {
                    // The transcript started over meanwhile: this page is of another life.
                    return;
                }
                this.earlier.loading = false;
                match page {
                    Ok(page) => {
                        for event in &page.events {
                            this.note_attachments(event);
                        }
                        this.transcript.prepend(&page.events, !page.has_more);
                        if let Some(first) = page.events.first() {
                            this.earlier.first_seq = Some(first.seq);
                        }
                        // A page without events ends the paging whatever it says.
                        this.earlier.more = page.has_more && !page.events.is_empty();
                    }
                    Err(error) => {
                        this.error =
                            Some(format!("Could not load earlier messages: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn input(&self) -> &TextInput {
        &self.input
    }

    pub fn vim(&self) -> &Vim {
        &self.vim
    }

    /// Feeds a key to Vim's modal layer over the draft.
    pub fn vim_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        options: Options,
        cx: &mut Context<Self>,
    ) -> Outcome {
        let outcome =
            crate::workbench::vim_ui::apply(&mut self.vim, &mut self.input, event, options, cx);
        cx.notify();
        outcome
    }

    pub fn chips(&self) -> &[Chip] {
        &self.chips
    }

    /// The structured context attached to the next message.
    pub fn context(&self) -> &[ContextChip] {
        &self.context
    }

    /// What the daemon resolved for the attachments of the user message `item` (kind, label,
    /// characters kept, whether the budget cut it); empty for a message without any.
    pub fn attachments_of(&self, item: ItemId) -> &[AttachmentMeta] {
        self.attachments.get(&item).map_or(&[], Vec::as_slice)
    }

    fn note_attachments(&mut self, event: &AgentEvent) {
        let Some(item) = event.item_id else {
            return;
        };
        let meta = attachments_meta(event);
        if !meta.is_empty() {
            self.attachments.insert(item, meta);
        }
    }

    pub fn sending(&self) -> bool {
        self.sending
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn clear_error(&mut self, cx: &mut Context<Self>) {
        if self.error.take().is_some() {
            cx.notify();
        }
    }

    // ---- composer --------------------------------------------------------------------------

    /// Puts back the draft a previous window or tab left, unless something has been typed here
    /// already.
    pub fn restore_draft(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.input.is_empty() && !text.is_empty() {
            self.input.reset_to(text);
            cx.notify();
        }
    }

    /// Puts `note` in the composer for the user to send (after a restore of files, CK-5): the
    /// whole draft when it is empty, else after what was typed.
    pub fn add_note(&mut self, note: &str, cx: &mut Context<Self>) {
        // A second restore of the same files says the same: once is enough.
        if self.input.text().contains(note) {
            return;
        }
        let text = if self.input.is_empty() {
            note.to_owned()
        } else {
            format!("{}\n\n{note}", self.input.text())
        };
        self.input.reset_to(&text);
        cx.notify();
    }

    /// Edits the draft.
    pub fn edit(&mut self, change: impl FnOnce(&mut TextInput), cx: &mut Context<Self>) {
        change(&mut self.input);
        cx.notify();
    }

    /// Edits the draft as a text field: `f` gets its text and, with `vim` on, its Vim state.
    pub fn edit_field<R>(
        &mut self,
        vim: bool,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut FieldMut<'_>, &mut Context<Self>) -> R,
    ) -> R {
        let out = f(
            &mut FieldMut {
                input: &mut self.input,
                vim: vim.then_some(&mut self.vim),
                multiline: true,
            },
            cx,
        );
        cx.notify();
        out
    }

    /// Attaches a terminal block to the next message.
    pub fn attach(&mut self, chip: Chip, cx: &mut Context<Self>) {
        self.chips.push(chip);
        cx.notify();
    }

    /// Attaches structured context to the next message; the daemon reads, redacts and bounds it
    /// when the message is sent. At most [`hutan_protocol::assistant::ATTACHMENTS_MAX`].
    pub fn attach_context(&mut self, chip: ContextChip, cx: &mut Context<Self>) -> bool {
        if self.context.len() >= hutan_protocol::assistant::ATTACHMENTS_MAX {
            self.error = Some(
                format!(
                    "At most {} attachments go with one message",
                    hutan_protocol::assistant::ATTACHMENTS_MAX
                )
                .into(),
            );
            cx.notify();
            return false;
        }
        self.context.push(chip);
        cx.notify();
        true
    }

    pub fn remove_context(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.context.len() {
            self.context.remove(index);
            cx.notify();
        }
    }

    pub fn remove_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.chips.len() {
            self.chips.remove(index);
            cx.notify();
        }
    }

    /// Takes the last attachment off the draft (the last chip drawn): `false` when there is none.
    pub fn pop_last_attachment(&mut self, cx: &mut Context<Self>) -> bool {
        let popped = self.context.pop().is_some() || self.chips.pop().is_some();
        if popped {
            cx.notify();
        }
        popped
    }

    /// Opens or closes the text of a folded paste.
    pub fn toggle_context_open(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(chip) = self.context.get_mut(index) {
            chip.expanded = !chip.expanded;
            cx.notify();
        }
    }

    /// Whether the draft has structured attachments.
    pub fn has_context(&self) -> bool {
        !self.context.is_empty()
    }

    /// The reason of the first chip that cannot be sent, if any.
    pub fn refused_context(&self) -> Option<&str> {
        self.context.iter().find_map(|chip| chip.refused.as_deref())
    }

    /// The message the composer would send now.
    pub fn draft_message(&self) -> String {
        compose(&self.chips, self.input.text())
    }

    /// Sends the draft with its chips and its structured context. Nothing happens when there is
    /// nothing to send or a send is in flight. The composer empties at once; a failure puts the
    /// draft back.
    pub fn send(&mut self, cx: &mut Context<Self>) -> bool {
        let message = self.draft_message();
        // An attachment can be the whole message: the daemon takes empty text with one.
        if (message.is_empty() && self.context.is_empty()) || self.sending {
            return false;
        }
        let draft = self.input.take();
        self.vim.reset();
        let chips = std::mem::take(&mut self.chips);
        let context = std::mem::take(&mut self.context);
        let attachments = context.iter().map(|c| c.attachment.clone()).collect();
        self.unsent = Some((draft, chips, context));
        self.sending = true;
        self.error = None;
        let connection = self.connection.clone();
        let session = self.session;
        cx.spawn(async move |this, cx| {
            let sent = connection
                .chat_send_with(session, message, attachments)
                .await;
            this.update(cx, |this, cx| {
                this.sending = false;
                let unsent = this.unsent.take();
                if let Err(error) = sent {
                    let refusal = refusal_of(&error);
                    let unsupported = unsupported_of(&error);
                    this.error = Some(
                        match (&refusal, &unsupported) {
                            (Some((_, message)), _) => format!("Not allowed: {message}"),
                            (None, Some(message)) => format!("Not supported: {message}"),
                            (None, None) => format!("Could not send: {error}"),
                        }
                        .into(),
                    );
                    // The user may have typed on meanwhile: the old draft goes in front.
                    if let Some((draft, chips, mut context)) = unsent {
                        let rest = this.input.take();
                        let mut restored = draft;
                        if !restored.is_empty() && !rest.is_empty() {
                            restored.push('\n');
                        }
                        restored.push_str(&rest);
                        this.input.set(&restored);
                        let mut all = chips;
                        all.append(&mut this.chips);
                        this.chips = all;
                        // A refusal says which kind of context the settings do not allow (a
                        // path outside the workspace names no setting: it is about files): the
                        // chips of that kind show it. An image the model cannot read marks the
                        // image chips.
                        if let Some((setting, message)) = refusal {
                            for chip in &mut context {
                                let about = match setting.as_deref() {
                                    Some(setting) => refused_by(setting, chip.kind()),
                                    None => chip.kind() == AttachmentKind::File,
                                };
                                if about {
                                    chip.refused = Some(message.clone());
                                }
                            }
                        }
                        if let Some(message) = unsupported {
                            for chip in &mut context {
                                if chip.kind() == AttachmentKind::Image {
                                    chip.refused = Some(message.clone());
                                }
                            }
                        }
                        context.append(&mut this.context);
                        this.context = context;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
        true
    }

    // ---- commands --------------------------------------------------------------------------

    /// `chat.interrupt` (Stop): the turn ends, and the queue is cleared unless `keep_queue`,
    /// which keeps it waiting. "Stopping…" until the turn has ended.
    pub fn interrupt(&mut self, keep_queue: bool, cx: &mut Context<Self>) {
        self.stopping = true;
        cx.notify();
        let connection = self.connection.clone();
        let session = self.session;
        cx.spawn(async move |this, cx| {
            let stopped = if connection.supports_chat_queue() {
                connection.chat_stop(session, keep_queue).await
            } else {
                connection.chat_interrupt(session).await
            };
            if let Err(error) = stopped {
                this.update(cx, |this, cx| {
                    this.stopping = false;
                    this.error = Some(format!("Could not interrupt: {error}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Stop was asked for and the turn has not ended yet.
    pub fn stopping(&self) -> bool {
        self.stopping
    }

    // ---- queue (protocol 0.36) ---------------------------------------------------------------

    /// Whether the daemon keeps a queue for chats (capability `chat.queue`).
    pub fn supports_queue(&self) -> bool {
        self.connection.supports_chat_queue()
    }

    /// The chat's queue as the daemon last said it.
    pub fn queue(&self) -> Option<&ChatQueue> {
        self.queue.as_ref()
    }

    /// The queued messages, in the order they are sent.
    pub fn queued(&self) -> &[QueuedMessage] {
        self.queue
            .as_ref()
            .map_or(&[], |queue| queue.items.as_slice())
    }

    /// The queued message open in the composer.
    pub fn editing(&self) -> Option<u64> {
        self.editing.as_ref().map(|editing| editing.item)
    }

    fn set_queue(&mut self, queue: ChatQueue, cx: &mut Context<Self>) {
        if let Some(editing) = &self.editing
            && !queue.items.iter().any(|item| item.id == editing.item)
        {
            // Sent or removed meanwhile: the edited text stays in the composer, ahead of what
            // was there before.
            if let Some(editing) = self.editing.take() {
                let mut text = self.input.take();
                if !editing.draft.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&editing.draft);
                }
                self.input.reset_to(&text);
            }
            self.error = Some("The queued message was sent before the edit was saved".into());
        }
        self.queue = Some(queue);
        cx.notify();
    }

    /// Takes the draft out of the composer for the queue or for Send now: its text with the
    /// terminal chips; `None` (with the reason shown) when it has nothing or carries
    /// attachments, which cannot wait in a queue.
    fn take_text_draft(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if self.sending {
            return None;
        }
        if !self.context.is_empty() {
            self.error =
                Some("Attachments cannot wait in the queue: send them once the turn ends".into());
            cx.notify();
            return None;
        }
        let message = self.draft_message();
        if message.trim().is_empty() {
            return None;
        }
        let draft = self.input.take();
        self.vim.reset();
        let chips = std::mem::take(&mut self.chips);
        self.unsent = Some((draft, chips, Vec::new()));
        self.sending = true;
        self.error = None;
        cx.notify();
        Some(message)
    }

    /// A send that failed: the draft goes back in front of what was typed meanwhile.
    fn put_back_unsent(&mut self) {
        let Some((draft, chips, context)) = self.unsent.take() else {
            return;
        };
        let rest = self.input.take();
        let mut restored = draft;
        if !restored.is_empty() && !rest.is_empty() {
            restored.push('\n');
        }
        restored.push_str(&rest);
        self.input.set(&restored);
        let mut all = chips;
        all.append(&mut self.chips);
        self.chips = all;
        let mut all = context;
        all.append(&mut self.context);
        self.context = all;
    }

    /// The send key while a turn runs: the draft waits at the end of the queue and goes when
    /// the turn has ended.
    pub fn queue_draft(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.take_text_draft(cx) else {
            return false;
        };
        let connection = self.connection.clone();
        let session = self.session;
        cx.spawn(async move |this, cx| {
            let queued = connection.chat_queue_add(session, message).await;
            this.update(cx, |this, cx| {
                this.sending = false;
                match queued {
                    Ok(_) => this.unsent = None,
                    Err(error) => {
                        this.put_back_unsent();
                        this.error = Some(format!("Could not queue: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        true
    }

    /// Send now: the draft goes without waiting for the turn (it steers the turn, or interrupts
    /// it and goes next; the transcript says which).
    pub fn send_draft_now(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.take_text_draft(cx) else {
            return false;
        };
        self.note_interrupting_send(cx);
        let connection = self.connection.clone();
        let session = self.session;
        cx.spawn(async move |this, cx| {
            let sent = connection.chat_send_now(session, Some(message), None).await;
            this.update(cx, |this, cx| {
                this.sending = false;
                match sent {
                    Ok(_) => this.unsent = None,
                    Err(error) => {
                        this.stopping = false;
                        this.put_back_unsent();
                        this.error = Some(format!("Could not send now: {error}").into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        true
    }

    /// Runs a queue request; a failure is shown as the last thing that went wrong.
    fn queue_request<T: 'static>(
        &mut self,
        what: &'static str,
        cx: &mut Context<Self>,
        request: impl AsyncFnOnce(Connection) -> Result<T, ClientError> + 'static,
    ) {
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            if let Err(error) = request(connection).await {
                this.update(cx, |this, cx| {
                    this.error = Some(format!("Could not {what}: {error}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// A queued message's Send now.
    pub fn send_queued_now(&mut self, item: u64, cx: &mut Context<Self>) {
        self.note_interrupting_send(cx);
        let session = self.session;
        let connection = self.connection.clone();
        cx.spawn(async move |this, cx| {
            if let Err(error) = connection.chat_send_now(session, None, Some(item)).await {
                this.update(cx, |this, cx| {
                    this.stopping = false;
                    this.error = Some(format!("Could not send it now: {error}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// A Send now that has to interrupt the running turn (the agent cannot take a message
    /// while it works) shows "Stopping…" like Stop does, until the turn has ended.
    fn note_interrupting_send(&mut self, cx: &mut Context<Self>) {
        let steers = self.queue.as_ref().is_some_and(|queue| queue.steer);
        if !steers && self.transcript.turn_active() {
            self.stopping = true;
            cx.notify();
        }
    }

    /// A queued message's ×.
    pub fn remove_queued(&mut self, item: u64, cx: &mut Context<Self>) {
        if self.editing() == Some(item) {
            self.cancel_edit(cx);
        }
        let session = self.session;
        self.queue_request("remove it", cx, async move |connection| {
            connection.chat_queue_remove(session, item).await
        });
    }

    /// The queue's Clear.
    pub fn clear_queue(&mut self, cx: &mut Context<Self>) {
        self.cancel_edit(cx);
        let session = self.session;
        self.queue_request("clear the queue", cx, async move |connection| {
            connection.chat_queue_clear(session).await
        });
    }

    /// A queued message's Edit: its text goes into the composer, where the send key saves it
    /// and Escape leaves it as it was. What the composer held comes back afterwards.
    pub fn edit_queued(&mut self, item: u64, cx: &mut Context<Self>) {
        let Some(text) = self
            .queued()
            .iter()
            .find(|queued| queued.id == item)
            .map(|queued| queued.text.clone())
        else {
            return;
        };
        let draft = match self.editing.take() {
            // Another edit was open: its draft is the one to come back.
            Some(open) => open.draft,
            None => self.input.text().to_owned(),
        };
        self.input.reset_to(&text);
        self.vim.reset();
        self.editing = Some(Editing { item, draft });
        self.error = None;
        cx.notify();
    }

    /// Saves the queued message open in the composer (an empty text removes it).
    pub fn save_edit(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editing.take() else {
            return false;
        };
        let text = self.input.text().to_owned();
        self.input.reset_to(&editing.draft);
        self.vim.reset();
        cx.notify();
        let session = self.session;
        let item = editing.item;
        if text.trim().is_empty() {
            self.queue_request("remove it", cx, async move |connection| {
                connection.chat_queue_remove(session, item).await
            });
        } else {
            self.queue_request("save the edit", cx, async move |connection| {
                connection.chat_queue_edit(session, item, text).await
            });
        }
        true
    }

    /// Leaves the queued message as it was; the composer gets its draft back.
    pub fn cancel_edit(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editing.take() else {
            return false;
        };
        self.input.reset_to(&editing.draft);
        self.vim.reset();
        cx.notify();
        true
    }

    /// Shows `message` as the last thing that went wrong.
    pub fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }
}

/// The setting (when the daemon names one) and the message of a `forbidden` refusal: a context
/// kind the daemon's `[assistant.context]` does not allow, a file outside the workspace.
fn refusal_of(error: &ClientError) -> Option<(Option<String>, String)> {
    let ClientError::Remote(body) = error else {
        return None;
    };
    if body.code != ErrorCode::Forbidden {
        return None;
    }
    let setting = body
        .data
        .as_ref()
        .and_then(|data| data.get("setting"))
        .and_then(|setting| setting.as_str())
        .map(str::to_owned);
    Some((setting, body.message.clone()))
}

/// The message of an `unsupported` refusal: the backend or model takes no images.
fn unsupported_of(error: &ClientError) -> Option<String> {
    let ClientError::Remote(body) = error else {
        return None;
    };
    (body.code == ErrorCode::Unsupported).then(|| body.message.clone())
}

/// The journal's newest events, oldest first. Without `stop_at` (the first read of a
/// transcript) that is one page of [`HISTORY_DEFAULT_LIMIT`] events; with it (a reload that
/// catches up with what happened meanwhile) pages are read back to, and including, the one that
/// holds `stop_at`, at most [`MAX_CATCH_UP_PAGES`] of them.
async fn fetch_history(
    connection: &Connection,
    session: SessionId,
    stop_at: Option<u64>,
) -> Result<History, ClientError> {
    let Some(stop_at) = stop_at else {
        let page = connection
            .chat_history(session, None, HISTORY_DEFAULT_LIMIT)
            .await?;
        return Ok(History {
            events: page.events,
            more_before: page.has_more,
        });
    };
    let mut pages: Vec<Vec<AgentEvent>> = Vec::new();
    let mut before = None;
    for _ in 0..MAX_CATCH_UP_PAGES {
        let page = connection
            .chat_history(session, before, HISTORY_MAX_LIMIT)
            .await?;
        let first = page.events.first().map(|event| event.seq);
        pages.push(page.events);
        match first {
            Some(first) if page.has_more && first > stop_at => before = Some(first),
            _ => break,
        }
    }
    pages.reverse();
    Ok(History {
        events: pages.into_iter().flatten().collect(),
        // Merging into a transcript that already has the older events leaves nothing missing.
        more_before: false,
    })
}
