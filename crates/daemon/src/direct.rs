//! Direct model chat (protocol 0.11, `docs/gateway.md`): a chat session whose "agent" is the
//! model gateway. There is no process: a turn is one streamed HTTP request to the session's
//! provider, mapped onto the same [`AgentEvent`]s a Codex session produces, so the pump, the
//! journal, the protocol events and every chat client work unchanged.
//!
//! The conversation is the session's own journal: before each turn the user and assistant
//! messages of `chat_items` are read back (oldest dropped beyond `gateway.max_context_chars`)
//! and sent with the new message. The model can be switched between turns; every turn records
//! the provider and model it used on its `turn_started` event.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use futures::StreamExt;
use hutan_agents::{AdapterMessage, ExitInfo};
use hutan_core::agent::{MessageRole, NoticeLevel, TokenCounts};
use hutan_core::{AgentEvent, AgentEventDraft, AgentEventKind, ItemId, SessionId, TurnId};
use hutan_gateway::{
    ChatMessage, ChatRequest, GatewayError, GatewayEvent, ImageContent, Provider, Role,
    is_length_cut,
};
use hutan_protocol::assistant::{ATTACHMENTS_META_KEY, AttachmentMeta};
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_storage::Storage;
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::context::Attached;
use crate::providers::ProviderService;

/// Most journal rows read to rebuild a conversation; far more than any context limit allows.
const HISTORY_ROWS: u32 = 4000;
/// How long what a provider said about a model's image input is remembered.
const VISION_TTL: std::time::Duration = std::time::Duration::from_secs(300);
/// Longest system prompt, in bytes.
pub const MAX_SYSTEM_PROMPT_BYTES: usize = 32 * 1024;

/// Which provider and model the next turn uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) provider_id: String,
    pub(crate) model: String,
    pub(crate) system_prompt: Option<String>,
}

#[derive(Debug)]
struct ActiveTurn {
    id: TurnId,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}

/// One running direct chat.
#[derive(Debug)]
pub(crate) struct DirectSession {
    id: SessionId,
    storage: Storage,
    providers: Arc<ProviderService>,
    tx: mpsc::Sender<AdapterMessage>,
    /// `gateway.max_context_chars`, shared with the chat manager (changed live).
    max_context_chars: Arc<AtomicUsize>,
    target: Mutex<Target>,
    turn: Mutex<Option<ActiveTurn>>,
    /// Cumulative token counts of the conversation (the `total` of `usage` events).
    usage: Mutex<TokenCounts>,
    /// Whether `(provider, model)` reads images, as the provider said and when.
    vision: Mutex<HashMap<(String, String), (Instant, bool)>>,
    shut: AtomicBool,
}

/// Adds `more` to `total`, field by field: a field neither side ever reported stays absent.
fn add_counts(total: &mut TokenCounts, more: &TokenCounts) {
    fn add(total: &mut Option<u64>, more: Option<u64>) {
        if let Some(more) = more {
            *total = Some(total.unwrap_or(0).saturating_add(more));
        }
    }
    add(&mut total.input_tokens, more.input_tokens);
    add(&mut total.cached_input_tokens, more.cached_input_tokens);
    add(&mut total.output_tokens, more.output_tokens);
    add(
        &mut total.reasoning_output_tokens,
        more.reasoning_output_tokens,
    );
    add(&mut total.total_tokens, more.total_tokens);
}

impl DirectSession {
    pub(crate) fn new(
        id: SessionId,
        storage: Storage,
        providers: Arc<ProviderService>,
        tx: mpsc::Sender<AdapterMessage>,
        max_context_chars: Arc<AtomicUsize>,
        target: Target,
        usage: TokenCounts,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            storage,
            providers,
            tx,
            max_context_chars,
            target: Mutex::new(target),
            turn: Mutex::new(None),
            usage: Mutex::new(usage),
            vision: Mutex::new(HashMap::new()),
            shut: AtomicBool::new(false),
        })
    }

    async fn emit(&self, draft: AgentEventDraft) {
        // A closed channel means the session is gone; nobody is left to tell.
        let _ = self.tx.send(AdapterMessage::Event(Box::new(draft))).await;
    }

    fn draft(turn: TurnId, item: Option<ItemId>, kind: AgentEventKind) -> AgentEventDraft {
        let mut draft = AgentEventDraft::new(kind);
        draft.turn_id = Some(turn);
        draft.item_id = item;
        draft
    }

    /// Starts a turn with the user's `text` and what they attached; its events follow on the
    /// session's stream. The attachments are part of the journaled message: the model gets, and
    /// the conversation keeps, `text` followed by their resolved text.
    ///
    /// `retry` (`chat.retry`): `text` is the journal's last user message; the request carries
    /// the conversation before it, then it, and the turn does not journal it again.
    pub(crate) async fn send(
        self: &Arc<Self>,
        text: String,
        attached: Option<Attached>,
        retry: bool,
    ) -> Result<TurnId, ErrorBody> {
        if self.shut.load(Ordering::Acquire) {
            return Err(ErrorBody::new(
                ErrorCode::InvalidState,
                "the chat session is shutting down",
            ));
        }
        let (text, attachments, images) = match attached {
            Some(attached) => (
                format!("{text}\n\n{}", attached.appendix),
                Some(attached.meta),
                attached.images,
            ),
            None => (text, None, Vec::new()),
        };
        let target = self.target.lock().clone();
        // Fails before anything is journaled when the provider is gone or lacks its key.
        let (record, provider) = self.providers.resolve(&target.provider_id).await?;
        if !images.is_empty() {
            self.require_vision(&record.id, &target.model, provider.as_ref())
                .await?;
        }
        let mut history = self.load_history().await?;
        if retry && let Some(last_user) = history.iter().rposition(|(role, _)| *role == Role::User)
        {
            // The message itself is the new one; what a failed turn said after it goes.
            history.truncate(last_user);
        }
        let max_context_chars = self.max_context_chars.load(Ordering::Relaxed);
        let (mut messages, dropped) = build_context(
            &history,
            &text,
            target.system_prompt.as_deref(),
            max_context_chars,
        );
        if let Some(message) = messages.last_mut() {
            // The images ride on the new message only: the journal keeps their headers, which
            // is all later turns send of them.
            message.images = images
                .into_iter()
                .map(|image| ImageContent {
                    media_type: image.media_type,
                    data: image.data,
                })
                .collect();
        }
        let request = ChatRequest::new(target.model.clone(), messages);

        let turn_id = TurnId::new();
        let cancel = CancellationToken::new();
        let mut slot = self.turn.lock();
        let task = tokio::spawn(self.clone().run_turn(Run {
            turn_id,
            provider,
            request,
            text,
            retry,
            attachments,
            provider_id: record.id,
            dropped,
            cancel: cancel.clone(),
        }));
        *slot = Some(ActiveTurn {
            id: turn_id,
            cancel,
            task,
        });
        Ok(turn_id)
    }

    /// Refuses images for a model the provider does not say reads them (`unsupported`). The
    /// provider's answer is remembered for a few minutes.
    async fn require_vision(
        &self,
        provider_id: &str,
        model: &str,
        provider: &dyn Provider,
    ) -> Result<(), ErrorBody> {
        let key = (provider_id.to_owned(), model.to_owned());
        let known = self
            .vision
            .lock()
            .get(&key)
            .filter(|(at, _)| at.elapsed() < VISION_TTL)
            .map(|(_, answer)| *answer);
        let reads_images = match known {
            Some(answer) => answer,
            None => {
                let answer = provider
                    .supports_vision(model)
                    .await
                    .map_err(|e| crate::providers::gateway_error(&e))?;
                self.vision.lock().insert(key, (Instant::now(), answer));
                answer
            }
        };
        if reads_images {
            Ok(())
        } else {
            Err(ErrorBody::new(
                ErrorCode::Unsupported,
                format!(
                    "the model list of provider `{provider_id}` does not say that `{model}` accepts \
                     images: switch to a model that does, or send the message without them"
                ),
            ))
        }
    }

    /// The conversation so far from the journal, oldest first.
    async fn load_history(&self) -> Result<Vec<(Role, String)>, ErrorBody> {
        let storage = self.storage.clone();
        let id = self.id;
        let rows = tokio::task::spawn_blocking(move || {
            storage
                .chat()
                .items_of_kind(id, "message_end", HISTORY_ROWS)
        })
        .await
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}")))?;
        Ok(rows
            .iter()
            .filter_map(|row| serde_json::from_str::<AgentEvent>(&row.event_json).ok())
            .filter_map(|event| match event.kind {
                AgentEventKind::MessageEnd { role, text, .. } if !text.is_empty() => Some((
                    match role {
                        MessageRole::User => Role::User,
                        MessageRole::Assistant => Role::Assistant,
                    },
                    text,
                )),
                _ => None,
            })
            .collect())
    }

    /// Cancels the turn in progress; the turn ends with `turn_cancelled`.
    pub(crate) fn interrupt(&self) -> bool {
        match self.turn.lock().as_ref() {
            Some(turn) => {
                turn.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// The next turn uses `provider_id` / `model`; the journal gets a notice.
    pub(crate) async fn set_model(&self, provider_id: String, model: String) {
        let notice = {
            let mut target = self.target.lock();
            let notice = format!(
                "model switched from {} / {} to {provider_id} / {model}",
                target.provider_id, target.model
            );
            target.provider_id.clone_from(&provider_id);
            target.model.clone_from(&model);
            notice
        };
        debug!(session = %self.id, to = %model, "direct chat model switched");
        self.emit(AgentEventDraft::new(AgentEventKind::Notice {
            level: NoticeLevel::Info,
            message: notice,
        }))
        .await;
    }

    /// Ends the session: cancels a running turn, waits for its events to be queued, then
    /// reports the exit (which the pump turns into the session's end).
    pub(crate) async fn shutdown(&self) {
        self.shut.store(true, Ordering::Release);
        let turn = self.turn.lock().take();
        if let Some(turn) = turn {
            turn.cancel.cancel();
            if let Err(error) = turn.task.await {
                warn!(session = %self.id, %error, "a chat turn panicked");
            }
        }
        let _ = self
            .tx
            .send(AdapterMessage::Exited(ExitInfo {
                code: Some(0),
                signal: None,
                stderr_tail: String::new(),
            }))
            .await;
    }

    fn finish_turn(&self, id: TurnId) {
        let mut slot = self.turn.lock();
        if slot.as_ref().is_some_and(|t| t.id == id) {
            *slot = None;
        }
    }

    /// One turn: the user's message, the streamed reply, usage, and exactly one end event.
    async fn run_turn(self: Arc<Self>, run: Run) {
        let Run {
            turn_id,
            provider,
            request,
            text,
            retry,
            attachments,
            provider_id,
            dropped,
            cancel,
        } = run;
        let started = Instant::now();
        let model = request.model.clone();

        let mut start = Self::draft(turn_id, None, AgentEventKind::TurnStarted);
        start.payload = Some(json!({ "provider_id": provider_id, "model": model }));
        self.emit(start).await;

        if !retry {
            let user_item = ItemId::new();
            self.emit(Self::draft(
                turn_id,
                Some(user_item),
                AgentEventKind::MessageStart {
                    role: MessageRole::User,
                    phase: None,
                },
            ))
            .await;
            let mut user_end = Self::draft(
                turn_id,
                Some(user_item),
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    phase: None,
                    text,
                },
            );
            user_end.payload = attachments.map(|meta| json!({ ATTACHMENTS_META_KEY: meta }));
            self.emit(user_end).await;
        }
        if dropped > 0 {
            self.emit(Self::draft(
                turn_id,
                None,
                AgentEventKind::Notice {
                    level: NoticeLevel::Info,
                    message: format!(
                        "context truncated: the {dropped} oldest message(s) were left out \
                         (gateway.max_context_chars = {})",
                        self.max_context_chars.load(Ordering::Relaxed)
                    ),
                },
            ))
            .await;
        }

        let outcome = self
            .stream_reply(turn_id, provider.as_ref(), request, cancel)
            .await;

        if let Some((text, item)) = outcome.reply {
            self.emit(Self::draft(
                turn_id,
                Some(item),
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text,
                },
            ))
            .await;
        }
        if let Some(last) = outcome.usage {
            let total = {
                let mut total = self.usage.lock();
                add_counts(&mut total, &last);
                *total
            };
            self.emit(Self::draft(
                turn_id,
                None,
                AgentEventKind::Usage {
                    total: Some(total),
                    last: Some(last),
                    context_window: None,
                },
            ))
            .await;
        }
        if outcome.finish_reason.as_deref().is_some_and(is_length_cut) {
            self.emit(Self::draft(
                turn_id,
                None,
                AgentEventKind::Notice {
                    level: NoticeLevel::Warning,
                    message: "the reply was cut off at the token limit".to_owned(),
                },
            ))
            .await;
        }
        let end = match outcome.end {
            End::Completed => AgentEventKind::TurnCompleted {
                duration_ms: Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            },
            End::Cancelled => AgentEventKind::TurnCancelled,
            End::Failed(error) => AgentEventKind::TurnFailed {
                message: error.to_string(),
                details: Some(format!("provider `{provider_id}`, model `{model}`")),
            },
        };
        self.finish_turn(turn_id);
        self.emit(Self::draft(turn_id, None, end)).await;
    }

    /// Streams the provider's reply, emitting `message_start` and every delta; returns what
    /// is left to journal.
    async fn stream_reply(
        &self,
        turn_id: TurnId,
        provider: &dyn Provider,
        request: ChatRequest,
        cancel: CancellationToken,
    ) -> Outcome {
        let mut stream = provider.stream_chat(request, cancel);
        let item = ItemId::new();
        let mut reply = String::new();
        let mut started = false;
        let mut outcome = Outcome {
            reply: None,
            usage: None,
            finish_reason: None,
            end: End::Completed,
        };
        while let Some(next) = stream.next().await {
            match next {
                Ok(GatewayEvent::Delta(delta)) => {
                    if !started {
                        started = true;
                        self.emit(Self::draft(
                            turn_id,
                            Some(item),
                            AgentEventKind::MessageStart {
                                role: MessageRole::Assistant,
                                phase: None,
                            },
                        ))
                        .await;
                    }
                    reply.push_str(&delta);
                    self.emit(Self::draft(
                        turn_id,
                        Some(item),
                        AgentEventKind::MessageDelta { text: delta },
                    ))
                    .await;
                }
                Ok(GatewayEvent::Usage(usage)) => outcome.usage = Some(usage),
                Ok(GatewayEvent::Completed { finish_reason }) => {
                    outcome.finish_reason = finish_reason;
                    break;
                }
                Err(GatewayError::Cancelled) => {
                    outcome.end = End::Cancelled;
                    break;
                }
                Err(error) => {
                    outcome.end = End::Failed(error);
                    break;
                }
            }
        }
        // What was said before a failure or an interrupt is part of the conversation.
        if started {
            outcome.reply = Some((reply, item));
        }
        outcome
    }
}

/// Everything a turn needs, handed to its task.
struct Run {
    turn_id: TurnId,
    provider: Arc<dyn Provider>,
    request: ChatRequest,
    text: String,
    /// What the user's message says about its attachments (`attachments_meta`).
    attachments: Option<Vec<AttachmentMeta>>,
    /// `chat.retry`: the user message is in the journal already.
    retry: bool,
    provider_id: String,
    dropped: usize,
    cancel: CancellationToken,
}

enum End {
    Completed,
    Cancelled,
    Failed(GatewayError),
}

struct Outcome {
    /// The assistant's text (also after a failure) and its item id; `None` when no text came.
    reply: Option<(String, ItemId)>,
    usage: Option<TokenCounts>,
    finish_reason: Option<String>,
    end: End,
}

/// The messages of one request: the system prompt, then as much of the conversation as fits
/// `max_chars` (counting characters of `history` and the new message, newest first), then the
/// new user message. A kept conversation starts with a user message. Returns the messages and
/// how many history messages were left out.
pub(crate) fn build_context(
    history: &[(Role, String)],
    new_message: &str,
    system_prompt: Option<&str>,
    max_chars: usize,
) -> (Vec<ChatMessage>, usize) {
    let mut budget = max_chars.saturating_sub(new_message.chars().count());
    let mut keep = history.len();
    for (index, (_, text)) in history.iter().enumerate().rev() {
        let cost = text.chars().count();
        if cost > budget {
            break;
        }
        budget -= cost;
        keep = index;
    }
    // A conversation does not start with the assistant answering nobody.
    while keep < history.len() && history[keep].0 == Role::Assistant {
        keep += 1;
    }
    let mut messages = Vec::with_capacity(history.len() - keep + 2);
    if let Some(system) = system_prompt.filter(|s| !s.is_empty()) {
        messages.push(ChatMessage::new(Role::System, system));
    }
    messages.extend(
        history[keep..]
            .iter()
            .map(|(role, text)| ChatMessage::new(*role, text.clone())),
    );
    messages.push(ChatMessage::new(Role::User, new_message));
    (messages, keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(texts: &[(&str, &str)]) -> Vec<(Role, String)> {
        texts
            .iter()
            .map(|(role, text)| {
                (
                    if *role == "u" {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    (*text).to_owned(),
                )
            })
            .collect()
    }

    fn contents(messages: &[ChatMessage]) -> Vec<&str> {
        messages.iter().map(|m| m.content.as_str()).collect()
    }

    #[test]
    fn everything_fits_under_the_limit() {
        let h = history(&[("u", "hello"), ("a", "hi there")]);
        let (messages, dropped) = build_context(&h, "next", Some("be brief"), 1000);
        assert_eq!(dropped, 0);
        assert_eq!(
            contents(&messages),
            ["be brief", "hello", "hi there", "next"]
        );
        assert_eq!(messages[0].role, Role::System);
        assert_eq!(messages[3].role, Role::User);
    }

    #[test]
    fn the_oldest_messages_go_first_and_the_kept_part_starts_with_the_user() {
        // 5 + 8 + 5 + 8 characters of history, a 4-character new message.
        let h = history(&[
            ("u", "first"),
            ("a", "answer 1"),
            ("u", "again"),
            ("a", "answer 2"),
        ]);
        // Room for the new message plus the last two history messages (13): "again", "answer 2".
        let (messages, dropped) = build_context(&h, "next", None, 4 + 13);
        assert_eq!(dropped, 2);
        assert_eq!(contents(&messages), ["again", "answer 2", "next"]);
        // Room for the new message and only "answer 2": that is an assistant message first,
        // which is dropped too.
        let (messages, dropped) = build_context(&h, "next", None, 4 + 8);
        assert_eq!(dropped, 4);
        assert_eq!(contents(&messages), ["next"]);
    }

    #[test]
    fn the_new_message_is_always_sent_and_the_system_prompt_is_free() {
        let h = history(&[("u", "old")]);
        let (messages, dropped) = build_context(&h, "a long new message", Some("system"), 3);
        assert_eq!(dropped, 1);
        assert_eq!(contents(&messages), ["system", "a long new message"]);
    }

    #[test]
    fn counts_add_up_per_field() {
        let mut total = TokenCounts::default();
        add_counts(
            &mut total,
            &TokenCounts {
                input_tokens: Some(10),
                output_tokens: Some(2),
                ..TokenCounts::default()
            },
        );
        add_counts(
            &mut total,
            &TokenCounts {
                input_tokens: Some(30),
                cached_input_tokens: Some(5),
                ..TokenCounts::default()
            },
        );
        assert_eq!(total.input_tokens, Some(40));
        assert_eq!(total.output_tokens, Some(2));
        assert_eq!(total.cached_input_tokens, Some(5));
        assert_eq!(total.total_tokens, None);
    }
}
