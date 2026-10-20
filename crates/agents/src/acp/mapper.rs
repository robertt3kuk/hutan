//! ACP messages → [`AgentEventDraft`]s. A pure state machine: no I/O, so every update kind is
//! tested by feeding it JSON.
//!
//! ACP has no items with a start and an end: an assistant message is a run of
//! `agent_message_chunk`s, a thought a run of `agent_thought_chunk`s, and a tool call a
//! `tool_call` followed by `tool_call_update`s. The mapper turns runs into
//! `message_start/delta/end` and `reasoning_start/delta/end` (a run ends when something else
//! happens, its `messageId` changes, or the turn ends) and keeps what correlation the unified
//! model needs: tool call ids ↔ item ids, and the permission requests waiting for an answer.
//! A turn is the `session/prompt` request: it begins when [`Mapper::begin_turn`] is called and
//! ends with [`Mapper::completed`].

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Instant;

use hutan_core::agent::{
    FileChange, FileChangeKind, MAX_OUTPUT_BYTES, MAX_PAYLOAD_BYTES, MessageRole, NoticeLevel,
    PermissionKind, PermissionOption, PermissionOptionKind, PermissionOutcome, PlanStep,
    PlanStepStatus, ReasoningPart, TokenCounts, ToolKind, ToolStatus,
};
use hutan_core::question::{QuestionExpiry, QuestionField};
use hutan_core::{
    AgentEventDraft, AgentEventKind, ApprovalId, ItemId, QuestionId, QuestionOutcome,
    QuestionResponse, QuestionSpec, TurnId,
};
use serde_json::{Value, json};

use super::udiff;
use super::wire::PromptResult;
use crate::codex::{AnswerError, Mapped, Reply};
use crate::elicit::{self, Shape};
use crate::error::AgentError;

/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i64 = -32601;
/// Tool calls remembered; past this, finished ones are forgotten (oldest first).
const MAX_TOOLS: usize = 1024;

/// `session/update` kinds that carry nothing a conversation shows: the slash-command palette
/// and the title the agent gave the session.
const IGNORED: &[&str] = &["available_commands_update", "session_info_update"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenKind {
    Message,
    Reasoning,
}

#[derive(Debug)]
struct Open {
    kind: OpenKind,
    item: ItemId,
    message_id: Option<String>,
    text: String,
}

#[derive(Debug)]
struct TurnState {
    id: TurnId,
    started: Instant,
}

#[derive(Debug)]
struct Tool {
    item: ItemId,
    tool: ToolKind,
    name: String,
    started: Option<Instant>,
    ended: bool,
    /// Text content of the latest update (ACP updates replace the content they carry).
    text: String,
    /// Diffs already reported, by path and hash of the new content.
    diffs: HashSet<(String, u64)>,
}

#[derive(Debug)]
struct PendingPermission {
    rpc_id: Value,
    approval: ApprovalId,
    /// `(option id, the response result that choosing it sends)`.
    results: Vec<(String, Value)>,
    answered: Option<String>,
    /// The turn was cancelled: the agent was told so, nothing can be chosen any more.
    cancelled: bool,
}

/// An `elicitation/create` waiting for the user.
#[derive(Debug)]
struct PendingQuestion {
    rpc_id: Value,
    question: QuestionId,
    shape: Shape,
    fields: Vec<QuestionField>,
    /// What the user decided, once the response was sent.
    answered: Option<QuestionOutcome>,
    /// The turn was cancelled: the agent was told so, nothing can be answered any more.
    cancelled: bool,
}

/// What a finished prompt turn produced.
#[derive(Debug, Default)]
pub struct Finished {
    pub drafts: Vec<AgentEventDraft>,
    /// Permission requests that were still unanswered: the agent must be told they are moot.
    pub replies: Vec<Reply>,
    /// How the turn ended; `None` when no turn was in progress.
    pub outcome: Option<TurnOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug)]
pub struct Mapper {
    agent: String,
    session_id: String,
    turn: Option<TurnState>,
    open: Option<Open>,
    tools: HashMap<String, Tool>,
    tool_order: VecDeque<String>,
    /// By the JSON text of the request id.
    pending: HashMap<String, PendingPermission>,
    /// Elicitations, by the JSON text of the request id.
    questions: HashMap<String, PendingQuestion>,
    model: Option<String>,
    mode: Option<String>,
    /// The agent's working directory, named on every permission request.
    cwd: Option<String>,
}

impl Mapper {
    pub fn new(agent: &str, session_id: String) -> Self {
        Self {
            agent: agent.to_owned(),
            session_id,
            turn: None,
            open: None,
            tools: HashMap::new(),
            tool_order: VecDeque::new(),
            pending: HashMap::new(),
            questions: HashMap::new(),
            model: None,
            mode: None,
            cwd: None,
        }
    }

    /// Remembers the model and mode the session starts with, so that later changes are noticed.
    pub fn set_baseline(&mut self, model: Option<String>, mode: Option<String>) {
        self.model = model;
        self.mode = mode;
    }

    /// The directory the agent works in: permission requests say where they would act.
    pub fn set_cwd(&mut self, cwd: String) {
        self.cwd = Some(cwd);
    }

    fn draft(&self, item: Option<ItemId>, kind: AgentEventKind) -> AgentEventDraft {
        let mut draft = AgentEventDraft::new(kind);
        draft.provider.thread_id = Some(self.session_id.clone());
        draft.turn_id = self.turn.as_ref().map(|t| t.id);
        draft.item_id = item;
        draft
    }

    /// The event kind for a conversation that was just opened or resumed.
    pub fn resume_locator(&mut self, model: &str) -> AgentEventDraft {
        let mut draft = self.draft(
            None,
            AgentEventKind::ResumeLocator {
                agent: self.agent.clone(),
                thread_id: self.session_id.clone(),
                model: (!model.is_empty()).then(|| model.to_owned()),
            },
        );
        draft.turn_id = None;
        draft
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty() || !self.questions.is_empty()
    }

    // Turns -----------------------------------------------------------------------------------

    /// A turn begins with the user's message: the events that open it.
    pub fn begin_turn(&mut self, text: &str) -> (TurnId, Vec<AgentEventDraft>) {
        let id = TurnId::new();
        self.turn = Some(TurnState {
            id,
            started: Instant::now(),
        });
        let user = ItemId::new();
        let drafts = vec![
            self.draft(None, AgentEventKind::TurnStarted),
            self.draft(
                Some(user),
                AgentEventKind::MessageStart {
                    role: MessageRole::User,
                    phase: None,
                },
            ),
            self.draft(
                Some(user),
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    phase: None,
                    text: text.to_owned(),
                },
            ),
        ];
        (id, drafts)
    }

    /// The prompt could not be sent: the turn that was begun is over.
    pub fn fail_turn(&mut self, message: String) -> Vec<AgentEventDraft> {
        let mut drafts = self.close_open();
        drafts.extend(self.close_tools(false));
        let failed = self.draft(
            None,
            AgentEventKind::TurnFailed {
                message,
                details: None,
            },
        );
        drafts.push(failed);
        self.turn = None;
        drafts
    }

    /// The answer to `session/prompt`: the turn's last events. The result's `stopReason` says how
    /// it ended; an error response is a failed turn.
    pub fn completed(&mut self, result: Result<Value, AgentError>) -> Finished {
        let Some(started) = self.turn.as_ref().map(|t| t.started) else {
            return Finished::default();
        };
        let (stop, usage, error) = match result {
            Ok(value) => match serde_json::from_value::<PromptResult>(value.clone()) {
                Ok(parsed) => (parsed.stop_reason, parsed.usage, None),
                Err(error) => (
                    String::new(),
                    None,
                    Some(format!("the agent's prompt response is malformed: {error}")),
                ),
            },
            Err(error) => (String::new(), None, Some(error.to_string())),
        };
        let cancelled = stop == "cancelled";
        let mut drafts = self.close_open();
        // The agent was (or is about to be) told that nothing pending matters any more.
        let (expired, replies) = self.end_pending();
        drafts.extend(expired);
        drafts.extend(self.close_tools(!cancelled && error.is_none() && stop != "refusal"));
        if let Some(usage) = usage {
            let counts = TokenCounts {
                input_tokens: usage.input_tokens,
                cached_input_tokens: usage.cached_read_tokens,
                output_tokens: usage.output_tokens,
                reasoning_output_tokens: usage.thought_tokens,
                total_tokens: usage.total_tokens,
            };
            drafts.push(self.draft(
                None,
                AgentEventKind::Usage {
                    total: None,
                    last: Some(counts),
                    context_window: None,
                },
            ));
        }
        let (kind, outcome) = match (&error, stop.as_str()) {
            (Some(message), _) => (
                AgentEventKind::TurnFailed {
                    message: message.clone(),
                    details: None,
                },
                TurnOutcome::Failed,
            ),
            (None, "cancelled") => (AgentEventKind::TurnCancelled, TurnOutcome::Cancelled),
            (None, "refusal") => (
                AgentEventKind::TurnFailed {
                    message: "the agent refused to continue".to_owned(),
                    details: Some("stopReason: refusal".to_owned()),
                },
                TurnOutcome::Failed,
            ),
            (None, other) => {
                match other {
                    "max_tokens" => drafts.push(self.notice(
                        NoticeLevel::Warning,
                        "the turn stopped: the model reached its output limit".to_owned(),
                    )),
                    "max_turn_requests" => drafts.push(
                        self.notice(
                            NoticeLevel::Warning,
                            "the turn stopped: the agent reached its limit of model requests"
                                .to_owned(),
                        ),
                    ),
                    _ => {}
                }
                (
                    AgentEventKind::TurnCompleted {
                        duration_ms: u64::try_from(started.elapsed().as_millis()).ok(),
                    },
                    TurnOutcome::Completed,
                )
            }
        };
        drafts.push(self.draft(None, kind));
        self.turn = None;
        Finished {
            drafts,
            replies,
            outcome: Some(outcome),
        }
    }

    /// Ends the tool calls that never reported an end. `ok`: they are taken to have succeeded,
    /// otherwise they failed with their turn.
    fn close_tools(&mut self, ok: bool) -> Vec<AgentEventDraft> {
        let mut drafts = Vec::new();
        let open: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, t)| t.started.is_some() && !t.ended)
            .map(|(id, _)| id.clone())
            .collect();
        for id in open {
            let status = if ok {
                ToolStatus::Completed
            } else {
                ToolStatus::Failed
            };
            drafts.extend(self.end_tool(&id, status, None));
        }
        drafts
    }

    // Streaming runs ----------------------------------------------------------------------------

    fn close_open(&mut self) -> Vec<AgentEventDraft> {
        let Some(open) = self.open.take() else {
            return Vec::new();
        };
        let kind = match open.kind {
            OpenKind::Message => AgentEventKind::MessageEnd {
                role: MessageRole::Assistant,
                phase: None,
                text: open.text,
            },
            OpenKind::Reasoning => AgentEventKind::ReasoningEnd {
                summary: vec![open.text],
                content: Vec::new(),
            },
        };
        vec![self.draft(Some(open.item), kind)]
    }

    fn chunk(
        &mut self,
        kind: OpenKind,
        message_id: Option<String>,
        text: String,
    ) -> Vec<AgentEventDraft> {
        if text.is_empty() {
            return Vec::new();
        }
        let mut drafts = Vec::new();
        let continues = self
            .open
            .as_ref()
            .is_some_and(|open| open.kind == kind && open.message_id == message_id);
        if !continues {
            drafts.extend(self.close_open());
            let item = ItemId::new();
            let start = match kind {
                OpenKind::Message => AgentEventKind::MessageStart {
                    role: MessageRole::Assistant,
                    phase: None,
                },
                OpenKind::Reasoning => AgentEventKind::ReasoningStart,
            };
            drafts.push(self.draft(Some(item), start));
            self.open = Some(Open {
                kind,
                item,
                message_id,
                text: String::new(),
            });
        }
        let Some(open) = self.open.as_mut() else {
            return drafts;
        };
        open.text.push_str(&text);
        let item = open.item;
        let delta = match kind {
            OpenKind::Message => AgentEventKind::MessageDelta { text },
            OpenKind::Reasoning => AgentEventKind::ReasoningDelta {
                part: ReasoningPart::Summary,
                text,
            },
        };
        drafts.push(self.draft(Some(item), delta));
        drafts
    }

    // Updates -----------------------------------------------------------------------------------

    fn notice(&self, level: NoticeLevel, message: String) -> AgentEventDraft {
        self.draft(None, AgentEventKind::Notice { level, message })
    }

    fn opaque(&mut self, method: String, payload: Value) -> AgentEventDraft {
        let mut draft = self.draft(None, AgentEventKind::Opaque { method });
        draft.payload = bounded(payload);
        draft
    }

    /// A notification of the agent: `session/update`, or something Hutan has no model for.
    pub fn notification(&mut self, method: &str, params: Value) -> Vec<AgentEventDraft> {
        if method == "session/update" {
            self.update(params)
        } else {
            vec![self.opaque(method.to_owned(), params)]
        }
    }

    /// One `session/update` notification (its params).
    pub fn update(&mut self, params: Value) -> Vec<AgentEventDraft> {
        if let Some(session) = params.get("sessionId").and_then(Value::as_str)
            && session != self.session_id
        {
            return Vec::new();
        }
        let Some(update) = params.get("update") else {
            return vec![self.opaque("session/update".to_owned(), params)];
        };
        let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
            return vec![self.opaque("session/update".to_owned(), params)];
        };
        let kind = kind.to_owned();
        match kind.as_str() {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let message_id = update
                    .get("messageId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let text = update.get("content").and_then(block_text);
                let open = if kind == "agent_message_chunk" {
                    OpenKind::Message
                } else {
                    OpenKind::Reasoning
                };
                match text {
                    Some(text) => self.chunk(open, message_id, text),
                    None => vec![self.opaque(format!("session/update:{kind}"), params)],
                }
            }
            // The user's own words: Hutan journals them when it sends them.
            "user_message_chunk" => Vec::new(),
            "tool_call" => {
                let update = update.clone();
                self.tool_call(&update)
            }
            "tool_call_update" => {
                let update = update.clone();
                self.tool_call_update(&update)
            }
            "plan" => {
                let steps = update
                    .get("entries")
                    .and_then(Value::as_array)
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|entry| PlanStep {
                                text: entry
                                    .get("content")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                                status: match entry.get("status").and_then(Value::as_str) {
                                    Some("completed") => PlanStepStatus::Completed,
                                    Some("in_progress") => PlanStepStatus::InProgress,
                                    _ => PlanStepStatus::Pending,
                                },
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut drafts = self.close_open();
                drafts.push(self.draft(
                    None,
                    AgentEventKind::Plan {
                        explanation: None,
                        steps,
                        text: None,
                    },
                ));
                drafts
            }
            "usage_update" => {
                let number = |key: &str| update.get(key).and_then(Value::as_u64);
                let counts = TokenCounts {
                    total_tokens: number("used"),
                    ..TokenCounts::default()
                };
                vec![self.draft(
                    None,
                    AgentEventKind::Usage {
                        total: counts.total_tokens.map(|_| counts),
                        last: None,
                        context_window: number("size"),
                    },
                )]
            }
            "current_mode_update" => {
                let mode = update
                    .get("currentModeId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if mode.is_some() && mode != self.mode {
                    let message = format!("mode: {}", mode.as_deref().unwrap_or_default());
                    self.mode = mode;
                    vec![self.notice(NoticeLevel::Info, message)]
                } else {
                    Vec::new()
                }
            }
            "config_option_update" => self.config_options_changed(update),
            other if IGNORED.contains(&other) => Vec::new(),
            _ => vec![self.opaque(format!("session/update:{kind}"), params)],
        }
    }

    /// The agent changed a session setting: the model and the mode are announced.
    fn config_options_changed(&mut self, update: &Value) -> Vec<AgentEventDraft> {
        let mut notices = Vec::new();
        let options = update
            .get("configOptions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for option in options {
            let category = option.get("category").and_then(Value::as_str);
            let id = option.get("id").and_then(Value::as_str);
            let value = option
                .get("currentValue")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let Some(value) = value else { continue };
            if category == Some("model") || id == Some("model") {
                if self.model.as_ref() != Some(&value) {
                    notices.push(format!("model: {value}"));
                    self.model = Some(value);
                }
            } else if (category == Some("mode") || id == Some("mode"))
                && self.mode.as_ref() != Some(&value)
            {
                notices.push(format!("mode: {value}"));
                self.mode = Some(value);
            }
        }
        notices
            .into_iter()
            .map(|message| self.notice(NoticeLevel::Info, message))
            .collect()
    }

    // Tool calls --------------------------------------------------------------------------------

    fn remember_tool(&mut self, id: &str, tool: Tool) {
        if self.tools.len() >= MAX_TOOLS {
            while let Some(old) = self.tool_order.pop_front() {
                if self.tools.get(&old).is_some_and(|t| t.ended) {
                    self.tools.remove(&old);
                    break;
                }
            }
        }
        self.tool_order.push_back(id.to_owned());
        self.tools.insert(id.to_owned(), tool);
    }

    /// The item id of a tool call, created on first sight (a permission request can come
    /// before the call itself).
    fn tool_item(&mut self, id: &str, kind: ToolKind) -> ItemId {
        if let Some(tool) = self.tools.get(id) {
            return tool.item;
        }
        let item = ItemId::new();
        self.remember_tool(
            id,
            Tool {
                item,
                tool: kind,
                name: String::new(),
                started: None,
                ended: false,
                text: String::new(),
                diffs: HashSet::new(),
            },
        );
        item
    }

    fn tool_call(&mut self, update: &Value) -> Vec<AgentEventDraft> {
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return vec![self.opaque("session/update:tool_call".to_owned(), update.clone())];
        };
        let mut drafts = self.close_open();
        drafts.extend(self.start_tool(id, update));
        // A call can arrive finished, or with content already.
        drafts.extend(self.tool_progress(id, update));
        drafts
    }

    fn tool_call_update(&mut self, update: &Value) -> Vec<AgentEventDraft> {
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return vec![self.opaque("session/update:tool_call_update".to_owned(), update.clone())];
        };
        let mut drafts = Vec::new();
        if self.tools.get(id).is_none_or(|t| t.started.is_none()) {
            // An update of a call we never saw start: the call begins here.
            drafts.extend(self.close_open());
            drafts.extend(self.start_tool(id, update));
        }
        drafts.extend(self.tool_progress(id, update));
        drafts
    }

    /// `tool_start` for a call, once.
    fn start_tool(&mut self, id: &str, update: &Value) -> Vec<AgentEventDraft> {
        let kind = tool_kind(update.get("kind").and_then(Value::as_str));
        let item = self.tool_item(id, kind);
        if self.tools.get(id).is_some_and(|t| t.started.is_some()) {
            return Vec::new();
        }
        let title = update
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or("tool call")
            .to_owned();
        // Zed's Claude Code adapter names the tool in `_meta`; other agents give the ACP kind.
        let name = update
            .pointer("/_meta/claudeCode/toolName")
            .or_else(|| update.pointer("/_meta/toolName"))
            .and_then(Value::as_str)
            .or_else(|| update.get("kind").and_then(Value::as_str))
            .unwrap_or("tool")
            .to_owned();
        if let Some(tool) = self.tools.get_mut(id) {
            tool.tool = kind;
            tool.name.clone_from(&name);
            tool.started = Some(Instant::now());
        }
        let input = update.get("rawInput").cloned().and_then(bounded);
        vec![self.draft(
            Some(item),
            AgentEventKind::ToolStart {
                tool: kind,
                name,
                title,
                input,
            },
        )]
    }

    /// What an update says about a call that has started: new content, diffs, an end.
    fn tool_progress(&mut self, id: &str, update: &Value) -> Vec<AgentEventDraft> {
        let mut drafts = Vec::new();
        let Some(tool) = self.tools.get_mut(id) else {
            return drafts;
        };
        let item = tool.item;
        let content = update.get("content").and_then(Value::as_array);
        let mut changes = Vec::new();
        let mut texts = Vec::new();
        for entry in content.map(Vec::as_slice).unwrap_or_default() {
            match entry.get("type").and_then(Value::as_str) {
                Some("diff") => {
                    if let Some((change, key)) = diff_change(entry)
                        && tool.diffs.insert(key)
                    {
                        changes.push(change);
                    }
                }
                Some("content") => {
                    if let Some(text) = entry.get("content").and_then(block_text) {
                        texts.push(text);
                    }
                }
                _ => {}
            }
        }
        let text = texts.join("\n");
        let text_changed = !text.is_empty() && text != tool.text;
        if !text.is_empty() {
            tool.text.clone_from(&text);
        }
        if !changes.is_empty() {
            drafts.push(self.draft(
                Some(item),
                AgentEventKind::FileDiff {
                    changes,
                    patch: None,
                },
            ));
        }
        let status = update.get("status").and_then(Value::as_str);
        match status {
            Some("completed" | "failed") => {
                let tool_status = if status == Some("failed") {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Completed
                };
                let raw = update.get("rawOutput");
                drafts.extend(self.end_tool(id, tool_status, raw));
            }
            _ if text_changed => {
                let message = truncated(text).0;
                drafts.push(self.draft(Some(item), AgentEventKind::ToolUpdate { message }));
            }
            _ => {}
        }
        drafts
    }

    fn end_tool(
        &mut self,
        id: &str,
        status: ToolStatus,
        raw_output: Option<&Value>,
    ) -> Vec<AgentEventDraft> {
        let Some(tool) = self.tools.get_mut(id) else {
            return Vec::new();
        };
        if tool.ended || tool.started.is_none() {
            return Vec::new();
        }
        tool.ended = true;
        let item = tool.item;
        let kind = tool.tool;
        let duration_ms = tool
            .started
            .and_then(|at| u64::try_from(at.elapsed().as_millis()).ok());
        let output = if tool.text.is_empty() {
            raw_output.and_then(raw_text)
        } else {
            Some(tool.text.clone())
        };
        let exit_code = raw_output.and_then(exit_code);
        let (output, truncated) = match output {
            Some(text) => {
                let (text, cut) = truncated(text);
                (Some(text), cut)
            }
            None => (None, false),
        };
        vec![self.draft(
            Some(item),
            AgentEventKind::ToolEnd {
                tool: kind,
                status,
                output,
                truncated,
                exit_code,
                duration_ms,
            },
        )]
    }

    // Permission requests -----------------------------------------------------------------------

    /// A request of the agent. `session/request_permission` becomes a pending approval, answered
    /// through [`answer`](Self::answer), and `elicitation/create` (form mode, which is all the
    /// handshake offers) a pending question, answered through
    /// [`answer_question`](Self::answer_question); everything else is refused, so the agent never
    /// waits for a client that cannot answer (Hutan serves no file system and no terminal).
    pub fn request(&mut self, id: Value, method: &str, params: Value) -> Mapped {
        if !matches!(method, "session/request_permission" | "elicitation/create") {
            let notice = self.notice(
                NoticeLevel::Warning,
                format!("the agent sent `{method}`, which Hutan does not offer; it was declined"),
            );
            return Mapped {
                drafts: vec![notice],
                reply: Some(Reply {
                    id,
                    outcome: Err((
                        METHOD_NOT_FOUND,
                        format!("`{method}` is not supported by this client"),
                    )),
                }),
            };
        }
        if params
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|s| s != self.session_id)
        {
            return Mapped {
                drafts: Vec::new(),
                reply: Some(Reply {
                    id,
                    outcome: Err((
                        METHOD_NOT_FOUND,
                        "request for an unknown session".to_owned(),
                    )),
                }),
            };
        }
        if method == "elicitation/create" {
            return self.elicitation(id, params);
        }
        self.permission(id, params)
    }

    fn permission(&mut self, rpc_id: Value, params: Value) -> Mapped {
        let tool_call = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let kind = match tool_call.get("kind").and_then(Value::as_str) {
            Some("execute") => PermissionKind::Command,
            Some("edit" | "delete" | "move") => PermissionKind::FileChange,
            _ => PermissionKind::Other,
        };
        let title = tool_call
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or("permission requested")
            .to_owned();
        let detail = permission_detail(self.cwd.as_deref(), &tool_call);

        let mut options = Vec::new();
        let mut results = Vec::new();
        for option in params
            .get("options")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let Some(option_id) = option.get("optionId").and_then(Value::as_str) else {
                continue;
            };
            options.push(PermissionOption {
                id: option_id.to_owned(),
                label: option
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(option_id)
                    .to_owned(),
                kind: match option.get("kind").and_then(Value::as_str) {
                    Some("allow_once") => PermissionOptionKind::AllowOnce,
                    Some("allow_always") => PermissionOptionKind::AllowAlways,
                    Some("reject_once") => PermissionOptionKind::RejectOnce,
                    Some("reject_always") => PermissionOptionKind::RejectAlways,
                    _ => PermissionOptionKind::Other,
                },
                provider: Some(option.clone()),
            });
            results.push((
                option_id.to_owned(),
                json!({"outcome": {"outcome": "selected", "optionId": option_id}}),
            ));
        }
        if options.is_empty() {
            // Nothing could be chosen: the agent is told so right away.
            return Mapped {
                drafts: vec![self.notice(
                    NoticeLevel::Warning,
                    format!("the agent asked permission (`{title}`) without offering an option; it was cancelled"),
                )],
                reply: Some(Reply {
                    id: rpc_id,
                    outcome: Ok(json!({"outcome": {"outcome": "cancelled"}})),
                }),
            };
        }
        let item = tool_call
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(|id| self.tool_item(id, tool_kind(tool_call.get("kind").and_then(Value::as_str))));
        let approval = ApprovalId::new();
        let mut drafts = self.close_open();
        let mut draft = self.draft(
            item,
            AgentEventKind::PermissionRequested {
                approval_id: approval,
                kind,
                title,
                detail,
                options,
            },
        );
        draft.provider.request_id = Some(rpc_id.to_string());
        draft.payload = bounded(params);
        drafts.push(draft);
        self.pending.insert(
            rpc_id.to_string(),
            PendingPermission {
                rpc_id,
                approval,
                results,
                answered: None,
                cancelled: false,
            },
        );
        Mapped {
            drafts,
            reply: None,
        }
    }

    /// Picks the response for `option_id` of a pending request and marks it answered. Returns the
    /// JSON-RPC id to answer and the result to send.
    pub fn answer(
        &mut self,
        approval: ApprovalId,
        option_id: &str,
    ) -> Result<(Value, Value), AnswerError> {
        let pending = self
            .pending
            .values_mut()
            .find(|p| p.approval == approval && p.answered.is_none() && !p.cancelled)
            .ok_or(AnswerError::Unknown)?;
        let result = pending
            .results
            .iter()
            .find(|(id, _)| id == option_id)
            .map(|(_, result)| result.clone())
            .ok_or_else(|| AnswerError::NotOffered(option_id.to_owned()))?;
        pending.answered = Some(option_id.to_owned());
        Ok((pending.rpc_id.clone(), result))
    }

    /// Undoes [`answer`](Self::answer) when the response could not be sent.
    pub fn unanswer(&mut self, approval: ApprovalId) {
        if let Some(pending) = self.pending.values_mut().find(|p| p.approval == approval) {
            pending.answered = None;
        }
    }

    /// The answer to `approval` reached the agent: the request is resolved.
    pub fn answered(&mut self, approval: ApprovalId) -> Option<AgentEventDraft> {
        let key = self
            .pending
            .iter()
            .find(|(_, p)| p.approval == approval)
            .map(|(key, _)| key.clone())?;
        let pending = self.pending.remove(&key)?;
        Some(self.resolved_draft(&pending))
    }

    fn elicitation(&mut self, rpc_id: Value, params: Value) -> Mapped {
        let mode = params.get("mode").and_then(Value::as_str).unwrap_or("form");
        if mode != "form" {
            // The handshake offers form mode only: a url elicitation is the agent's mistake.
            return Mapped {
                drafts: vec![self.notice(
                    NoticeLevel::Warning,
                    format!("the agent sent a `{mode}` elicitation, which Hutan does not offer; it was declined"),
                )],
                reply: Some(Reply {
                    id: rpc_id,
                    outcome: Ok(json!({"action": "decline"})),
                }),
            };
        }
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let form = elicit::parse(params.get("requestedSchema").unwrap_or(&Value::Null));
        let in_turn = self.turn.is_some();
        let (title, prompt) = split_message(&message);
        let spec = QuestionSpec {
            title,
            prompt,
            fields: form.fields.clone(),
            raw_schema: form.raw_schema,
            source: None,
            expires: if in_turn {
                QuestionExpiry::Turn
            } else {
                QuestionExpiry::Process
            },
        };
        let item = params
            .get("toolCallId")
            .and_then(Value::as_str)
            .map(|id| self.tool_item(id, ToolKind::Other));
        let question = QuestionId::new();
        let mut drafts = self.close_open();
        let mut draft = self.draft(
            item,
            AgentEventKind::QuestionRequested {
                question_id: question,
                spec,
            },
        );
        draft.provider.request_id = Some(rpc_id.to_string());
        draft.payload = bounded(params);
        drafts.push(draft);
        self.questions.insert(
            rpc_id.to_string(),
            PendingQuestion {
                rpc_id,
                question,
                shape: form.shape,
                fields: form.fields,
                answered: None,
                cancelled: false,
            },
        );
        Mapped {
            drafts,
            reply: None,
        }
    }

    /// The response to send for the user's decision on a pending question; marks it answered.
    /// Returns the JSON-RPC id to answer and the result to send.
    pub fn answer_question(
        &mut self,
        question: QuestionId,
        response: &QuestionResponse,
    ) -> Result<(Value, Value), AnswerError> {
        let pending = self
            .questions
            .values_mut()
            .find(|p| p.question == question && p.answered.is_none() && !p.cancelled)
            .ok_or(AnswerError::Unknown)?;
        let (result, outcome) = match response {
            QuestionResponse::Submit { values } => {
                let content = elicit::content(pending.shape, &pending.fields, values)
                    .map_err(AnswerError::Invalid)?;
                (
                    json!({"action": "accept", "content": content}),
                    QuestionOutcome::answered(&pending.fields, values),
                )
            }
            QuestionResponse::Decline => (json!({"action": "decline"}), QuestionOutcome::Declined),
            QuestionResponse::Cancel => (json!({"action": "cancel"}), QuestionOutcome::Cancelled),
        };
        pending.answered = Some(outcome);
        Ok((pending.rpc_id.clone(), result))
    }

    /// Undoes [`answer_question`](Self::answer_question) when the response could not be sent.
    pub fn unanswer_question(&mut self, question: QuestionId) {
        if let Some(pending) = self.questions.values_mut().find(|p| p.question == question) {
            pending.answered = None;
        }
    }

    /// The answer to `question` reached the agent: ACP has no "resolved" notification, so the
    /// question is resolved now.
    pub fn question_answered(&mut self, question: QuestionId) -> Option<AgentEventDraft> {
        let key = self
            .questions
            .iter()
            .find(|(_, p)| p.question == question)
            .map(|(key, _)| key.clone())?;
        let pending = self.questions.remove(&key)?;
        Some(self.question_resolved_draft(&pending))
    }

    fn question_resolved_draft(&self, pending: &PendingQuestion) -> AgentEventDraft {
        let mut draft = self.draft(
            None,
            AgentEventKind::QuestionResolved {
                question_id: pending.question,
                outcome: pending.answered.clone().unwrap_or(QuestionOutcome::Expired),
                // The daemon knows who answered; the agent does not.
                decided_by: None,
            },
        );
        draft.provider.request_id = Some(pending.rpc_id.to_string());
        draft
    }

    /// The model was switched by the user: the notice that says so, unless the agent already
    /// announced it.
    pub fn model_switched(&mut self, model: &str) -> Option<AgentEventDraft> {
        if self.model.as_deref() == Some(model) {
            return None;
        }
        self.model = Some(model.to_owned());
        Some(self.notice(NoticeLevel::Info, format!("model: {model}")))
    }

    /// The mode was switched by the user.
    pub fn mode_switched(&mut self, mode: &str) -> Option<AgentEventDraft> {
        if self.mode.as_deref() == Some(mode) {
            return None;
        }
        self.mode = Some(mode.to_owned());
        Some(self.notice(NoticeLevel::Info, format!("mode: {mode}")))
    }

    /// The user interrupts: every request still waiting is answered `cancelled` (ACP says the
    /// client must when it cancels a turn). The requests stay known until the turn ends, then
    /// expire.
    pub fn cancel_pending(&mut self) -> Vec<Reply> {
        let mut replies = Vec::new();
        for pending in self.pending.values_mut() {
            if pending.answered.is_none() && !pending.cancelled {
                pending.cancelled = true;
                replies.push(Reply {
                    id: pending.rpc_id.clone(),
                    outcome: Ok(json!({"outcome": {"outcome": "cancelled"}})),
                });
            }
        }
        for pending in self.questions.values_mut() {
            if pending.answered.is_none() && !pending.cancelled {
                pending.cancelled = true;
                replies.push(Reply {
                    id: pending.rpc_id.clone(),
                    outcome: Ok(json!({"action": "cancel"})),
                });
            }
        }
        replies
    }

    fn resolved_draft(&self, pending: &PendingPermission) -> AgentEventDraft {
        let outcome = match &pending.answered {
            Some(option_id) => PermissionOutcome::Selected {
                option_id: option_id.clone(),
            },
            None => PermissionOutcome::Expired,
        };
        let mut draft = self.draft(
            None,
            AgentEventKind::PermissionResolved {
                approval_id: pending.approval,
                outcome,
                // The daemon knows who answered; the agent does not.
                decided_by: None,
            },
        );
        draft.provider.request_id = Some(pending.rpc_id.to_string());
        draft
    }

    /// The turn is over: what was answered is resolved, what was not expires, and the agent is
    /// told (`cancelled`) about the requests it still waits on.
    fn end_pending(&mut self) -> (Vec<AgentEventDraft>, Vec<Reply>) {
        let pending: Vec<PendingPermission> = self.pending.drain().map(|(_, p)| p).collect();
        let mut replies = Vec::new();
        let mut drafts = Vec::new();
        for p in &pending {
            if p.answered.is_none() && !p.cancelled {
                replies.push(Reply {
                    id: p.rpc_id.clone(),
                    outcome: Ok(json!({"outcome": {"outcome": "cancelled"}})),
                });
            }
            drafts.push(self.resolved_draft(p));
        }
        let asked: Vec<PendingQuestion> = self.questions.drain().map(|(_, q)| q).collect();
        for q in &asked {
            if q.answered.is_none() && !q.cancelled {
                replies.push(Reply {
                    id: q.rpc_id.clone(),
                    outcome: Ok(json!({"action": "cancel"})),
                });
            }
            drafts.push(self.question_resolved_draft(q));
        }
        (drafts, replies)
    }

    /// The process is gone: nothing pending can be answered any more.
    pub fn expire_all(&mut self) -> Vec<AgentEventDraft> {
        let pending: Vec<PendingPermission> = self.pending.drain().map(|(_, p)| p).collect();
        let mut drafts: Vec<AgentEventDraft> =
            pending.iter().map(|p| self.resolved_draft(p)).collect();
        let asked: Vec<PendingQuestion> = self.questions.drain().map(|(_, q)| q).collect();
        drafts.extend(asked.iter().map(|q| self.question_resolved_draft(q)));
        drafts
    }
}

// Helpers -------------------------------------------------------------------------------------

/// An elicitation's `message` as the card's title and markdown body: a one-line message is the
/// title alone, otherwise its first line titles the rest.
fn split_message(message: &str) -> (String, String) {
    let message = message.trim();
    match message.split_once('\n') {
        None if message.is_empty() => ("The agent has a question".to_owned(), String::new()),
        None => (message.chars().take(120).collect(), String::new()),
        Some((first, rest)) => (
            first.trim().chars().take(120).collect(),
            rest.trim().to_owned(),
        ),
    }
}

/// What a permission request would act on: the working directory (`in <dir>`), then every file
/// it names, from the call's locations and its diffs, each once and in the order given. Nothing
/// is clamped: the user decides on all of it.
fn permission_detail(cwd: Option<&str>, tool_call: &Value) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
        lines.push(format!("in {cwd}"));
    }
    let located = tool_call
        .get("locations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|location| location.get("path").and_then(Value::as_str));
    let diffed = tool_call
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("diff"))
        .filter_map(|entry| entry.get("path").and_then(Value::as_str));
    let mut files: Vec<&str> = Vec::new();
    for path in located.chain(diffed) {
        if !path.is_empty() && !files.contains(&path) {
            files.push(path);
        }
    }
    lines.extend(files.iter().map(|path| (*path).to_owned()));
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The text of a content block; a stand-in line for what has none.
fn block_text(content: &Value) -> Option<String> {
    match content.get("type").and_then(Value::as_str)? {
        "text" => content
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "image" => Some("[image]".to_owned()),
        "audio" => Some("[audio]".to_owned()),
        "resource_link" => {
            let uri = content
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let name = content.get("name").and_then(Value::as_str).unwrap_or(uri);
            Some(format!("[{name}]({uri})"))
        }
        "resource" => content
            .pointer("/resource/text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                content
                    .pointer("/resource/uri")
                    .and_then(Value::as_str)
                    .map(|uri| format!("[resource {uri}]"))
            }),
        _ => None,
    }
}

fn tool_kind(kind: Option<&str>) -> ToolKind {
    match kind {
        Some("execute") => ToolKind::Command,
        Some("edit" | "delete" | "move") => ToolKind::FileChange,
        Some("fetch") => ToolKind::WebSearch,
        _ => ToolKind::Other,
    }
}

/// A `diff` content entry as a [`FileChange`] (and the key that tells it was reported).
fn diff_change(entry: &Value) -> Option<(FileChange, (String, u64))> {
    let path = entry.get("path").and_then(Value::as_str)?.to_owned();
    let old = entry.get("oldText").and_then(Value::as_str);
    let new = entry
        .get("newText")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut hasher = DefaultHasher::new();
    old.hash(&mut hasher);
    new.hash(&mut hasher);
    let key = (path.clone(), hasher.finish());
    let (kind, patch) = match old {
        None => (FileChangeKind::Add, new.to_owned()),
        Some(old) if new.is_empty() && !old.is_empty() => (FileChangeKind::Delete, old.to_owned()),
        Some(old) => (
            FileChangeKind::Update,
            udiff::unified(&path, old, new).unwrap_or_default(),
        ),
    };
    let (patch, _) = truncated(patch);
    Some((
        FileChange {
            path,
            kind,
            move_path: None,
            patch: Some(patch),
            before: None,
            after: None,
        },
        key,
    ))
}

/// What a tool's `rawOutput` says as text: a string, a well-known text member, else its JSON.
fn raw_text(raw: &Value) -> Option<String> {
    match raw {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Object(fields) => ["output", "stdout", "text", "content"]
            .iter()
            .find_map(|key| fields.get(*key).and_then(Value::as_str))
            .map(str::to_owned)
            .or_else(|| Some(raw.to_string())),
        other => Some(other.to_string()),
    }
}

fn exit_code(raw: &Value) -> Option<i32> {
    ["exit_code", "exitCode", "code"]
        .iter()
        .find_map(|key| raw.get(*key).and_then(Value::as_i64))
        .and_then(|code| i32::try_from(code).ok())
}

/// Cuts `text` to [`MAX_OUTPUT_BYTES`] on a character boundary.
fn truncated(mut text: String) -> (String, bool) {
    if text.len() <= MAX_OUTPUT_BYTES {
        return (text, false);
    }
    let mut end = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// A raw payload, unless it is larger than [`MAX_PAYLOAD_BYTES`].
fn bounded(value: Value) -> Option<Value> {
    (value.to_string().len() <= MAX_PAYLOAD_BYTES).then_some(value)
}
