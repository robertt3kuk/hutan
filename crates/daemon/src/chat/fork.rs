//! Fork a chat from any turn (ADR 0023, protocol 0.56, capability `chat.fork`).
//!
//! A fork is a new chat session that starts with the parent's conversation up to a turn. The
//! parent is never changed. Three things make it:
//!
//! 1. **The journal prefix is copied** into the child (`Storage::chat().fork_journal`: one
//!    transaction on the writer thread, the rows never leave SQLite), so either session can be
//!    deleted on its own. It happens at the child's first start, and is idempotent.
//! 2. **The agent's conversation is forked natively** where it can be: Codex `thread/fork`
//!    (cut after `lastTurnId`), ACP `session/fork` (OMP: at the end; Claude Code: at a message
//!    the adapter finds by the id the journal kept). The adapters say what they can do; this
//!    module asks only for what the parent's agent advertised, and never substitutes one cut
//!    for another.
//! 3. **`summary`** starts a fresh conversation and hands the agent a bounded digest of the
//!    kept turns with its first message. The digest is written by Hutan from the journal, never
//!    by a model call.
//!
//! [`ChatManager::plan_fork`] decides all of it before a session exists, so a refusal leaves
//! nothing behind; the registry adds the session and the first start does the rest.

use std::collections::{BTreeMap, HashSet};

use hutan_agents::{ForkFrom, ForkMessage, ForkPoint};
use hutan_core::agent::MessageRole;
use hutan_core::{ForkMode, ForkOrigin, ForkSupport};
use hutan_protocol::chat::{ChatFork, ForkWhere};
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::*;

/// Most events a fork copies. The copy is one transaction on the storage thread: a journal of
/// 50 000 events (about 100 MB) took about 2 s, so one this long is refused instead of holding
/// every other write behind it (`docs/agents.md` § Fork).
pub const FORK_MAX_EVENTS: u64 = 100_000;
/// Longest digest a summary fork hands over, in bytes.
pub const DIGEST_BUDGET: usize = 24 * 1024;
/// Newest assistant messages read to find the one a Claude fork cuts at (and how many equal
/// ones precede it).
const MESSAGE_LOOKBACK: u32 = 2000;
/// Newest messages and tool calls a digest reads.
const DIGEST_MESSAGES: u32 = 3000;
const DIGEST_TOOLS: u32 = 1000;
/// Messages of the journal read to find the measurement turns of a chat (newest first).
const MEASURE_SCAN: u32 = 5000;

/// Where a chat came from and what its first start still has to do (`ChatSpec.fork`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatForkSpec {
    /// The lineage: stays with the chat for good (`SessionInfo.forked_from`).
    pub origin: ForkOrigin,
    /// What the first start does; cleared once the agent is up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<ForkPending>,
    /// Summary forks: the digest the first message carries; cleared once it was delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<String>,
}

/// The first start of a fork.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkPending {
    /// The journal prefix to copy; `None` once copied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy: Option<JournalCopy>,
    /// Open the agent as a fork of this conversation; `None`: a fresh conversation (a direct
    /// chat, a summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ForkFrom>,
}

/// The journal prefix a fork copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalCopy {
    pub parent: SessionId,
    pub through_seq: u64,
    pub through_turn: String,
    /// The journal's own words about the fork, appended after the copy.
    pub notice: String,
}

/// What the registry knows of a chat that is to be forked.
#[derive(Debug)]
pub(crate) struct ForkParent {
    pub id: SessionId,
    pub title: String,
    pub workspace_id: hutan_core::WorkspaceId,
    /// The folder the chat works in.
    pub cwd: PathBuf,
    pub spec: ChatSpec,
    /// A turn is in progress, as the session's `ChatInfo` says.
    pub turn_active: bool,
    /// What its agent advertised when it last started in this daemon run; `None` before that.
    pub support: Option<ForkSupport>,
}

/// A fork that was decided, ready to become a session.
#[derive(Debug)]
pub(crate) struct ForkPlan {
    /// The child's spec: the parent's with the lineage and the first start.
    pub spec: ChatSpec,
    /// What the parent's agent can do, when it was known.
    pub support: Option<ForkSupport>,
    pub origin: ForkOrigin,
    /// Events the copy will make.
    pub copied_events: u64,
}

/// The conversation a fork opens its agent from, while its first start is still to come.
pub(super) fn fork_from(spec: &ChatSpec) -> Option<ForkFrom> {
    spec.fork
        .as_ref()
        .and_then(|fork| fork.pending.as_ref())
        .and_then(|pending| pending.from.clone())
}

/// What `ChatInfo.fork.note` says about a harness that does not cut anywhere (`None` for one
/// that does).
pub(crate) fn support_note(agent: &str, support: ForkSupport) -> Option<String> {
    match support {
        ForkSupport::Exact => None,
        ForkSupport::Head if agent == AGENT_CLAUDE => Some(
            "this Claude Code adapter takes no fork point (claude-agent-acp 0.98 or newer does): \
             it forks only at the chat's newest turn"
                .to_owned(),
        ),
        ForkSupport::Head => Some(format!(
            "{agent} forks only at its newest turn over ACP: an earlier turn needs a summary"
        )),
        ForkSupport::Summary => Some(format!(
            "{agent} does not offer session/fork: only a summary can seed a new chat"
        )),
        ForkSupport::None => Some("this chat cannot be forked".to_owned()),
    }
}

/// What a failed start says: an agent that cannot make the cut asked for is `unsupported` (the
/// error of a fork), not the `bad_request` an unsupported setting of a plain start is.
pub(super) fn forked_error(forking: bool, cause: &AgentError, error: ErrorBody) -> ErrorBody {
    if forking && matches!(cause, AgentError::Unsupported(_)) {
        ErrorBody::new(ErrorCode::Unsupported, cause.to_string())
    } else {
        error
    }
}

fn invalid_state(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidState, message)
}

/// `unsupported`, with what the harness can do and the cuts that would work.
fn unsupported(message: impl Into<String>, support: ForkSupport, at_head: bool) -> ErrorBody {
    let alternatives: Vec<&str> = [ForkMode::Head, ForkMode::Summary]
        .into_iter()
        .filter(|mode| support.allows(*mode, at_head))
        .map(ForkMode::as_str)
        .collect();
    let mut error = ErrorBody::new(ErrorCode::Unsupported, message);
    error.data = Some(json!({"support": support.as_str(), "alternatives": alternatives}));
    error
}

/// The journal's words about a fork.
fn notice(title: &str, mode: ForkMode, ordinal: u32, total: u32) -> String {
    match mode {
        ForkMode::Exact => {
            format!("Forked from “{title}” after turn {ordinal} of {total}.")
        }
        ForkMode::Head => format!(
            "Forked from “{title}” at its latest turn (turn {ordinal}): the agent's conversation \
             continues from there."
        ),
        ForkMode::Summary => format!(
            "Forked from “{title}” with a summary of turns 1–{ordinal}: the agent starts a new \
             conversation and is given a digest of them (written by Hutan, not by a model). \
             The history above is Hutan's copy of the journal."
        ),
    }
}

impl ChatManager {
    /// Whether a turn runs (or is being started) in the session.
    pub(crate) fn is_busy(&self, id: SessionId) -> bool {
        lock(&self.live)
            .get(&id)
            .is_some_and(|live| live.busy.load(Ordering::Acquire))
    }

    async fn read_storage<T: Send + 'static>(
        &self,
        read: impl FnOnce(Storage) -> hutan_storage::Result<T> + Send + 'static,
    ) -> Result<T, ErrorBody> {
        let storage = self.storage.clone();
        let internal = |e: &dyn std::fmt::Display| {
            ErrorBody::new(ErrorCode::Internal, format!("reading the journal: {e}"))
        };
        tokio::task::spawn_blocking(move || read(storage))
            .await
            .map_err(|e| internal(&e))?
            .map_err(|e| internal(&e))
    }

    /// Decides a fork of `parent` for `req`: which turn, how, what the agent is asked to do and
    /// what the journal copy is. Refuses — `invalid_state` for a running turn, an unknown turn
    /// or no turn; `unsupported` for a cut the harness cannot make (the `data` says which would
    /// work) — before anything exists.
    pub(crate) async fn plan_fork(
        &self,
        parent: &ForkParent,
        req: &ChatFork,
    ) -> Result<ForkPlan, ErrorBody> {
        let spec = &parent.spec;
        let direct = is_gateway_agent(&spec.agent);
        if parent.turn_active || self.is_busy(parent.id) {
            return Err(invalid_state(
                "a turn is in progress in this chat: wait for it to end, or interrupt it, before \
                 forking",
            ));
        }
        let id = parent.id;
        let turns = self.read_storage(move |s| s.chat().turns(id)).await?;
        // A measurement (`/context`, the rule of `hutan_core::insight::is_measure_message`) is no
        // turn of the conversation: never the latest turn to fork at, not counted in the
        // ordinals, not a turn to name.
        let measured = self.measurement_turns(id).await?;
        let real: Vec<usize> = (0..turns.len())
            .filter(|at| !measured.contains(&turns[*at].turn_id))
            .collect();
        let Some(&newest) = real.last() else {
            return Err(invalid_state(
                "this chat has no turn to fork at yet: send a message first",
            ));
        };
        let index = match req.turn_id {
            None => newest,
            Some(wanted) => {
                let wanted = wanted.to_string();
                if measured.contains(&wanted) {
                    return Err(invalid_state(format!(
                        "{wanted} is a measurement (`/context`), not a turn of the conversation"
                    )));
                }
                turns
                    .iter()
                    .position(|turn| turn.turn_id == wanted)
                    .ok_or_else(|| invalid_state(format!("{wanted} is not a turn of this chat")))?
            }
        };
        let at_head = index == newest;
        let ordinal_of = |at: usize| {
            u32::try_from(real.iter().take_while(|r| **r <= at).count()).unwrap_or(u32::MAX)
        };
        let ordinal = ordinal_of(index);
        let total = u32::try_from(real.len()).unwrap_or(u32::MAX);
        let turn = &turns[index];
        if turn.status == ChatTurnStatus::Running {
            return Err(invalid_state(format!(
                "turn {} is still running: a fork keeps only turns that have ended",
                turn.turn_id
            )));
        }
        let mode = req.mode;
        match req.where_ {
            ForkWhere::ThisFolder if req.branch.is_some() => {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    "`branch` names the branch of a new worktree: it needs `where: new_worktree`",
                ));
            }
            ForkWhere::NewWorktree if req.restore_files => {
                return Err(ErrorBody::new(
                    ErrorCode::BadRequest,
                    "a new worktree already has the files of the turn: `restore_files` is for a \
                     fork in this folder",
                ));
            }
            // Claude Code keeps its transcript with the folder it ran in: nothing can carry it
            // to a worktree, only a new conversation that is told the old one's gist.
            ForkWhere::NewWorktree if spec.agent == AGENT_CLAUDE && mode != ForkMode::Summary => {
                return Err(unsupported(
                    "Claude Code keeps a conversation with the folder it ran in: a fork in a new \
                     worktree is a new conversation given a summary of the turns (`summary`)",
                    ForkSupport::Summary,
                    at_head,
                ));
            }
            _ => {}
        }
        if mode == ForkMode::Head && req.turn_id.is_some() && !at_head {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "`head` forks at the latest turn: leave `turn_id` out, or use `exact` or \
                 `summary` for an earlier turn",
            ));
        }
        if mode == ForkMode::Summary && direct {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                "a direct chat forks exactly, from its own journal: a summary is for agents that \
                 cannot cut their conversation",
            ));
        }
        // What the agent said it can do; a direct chat is Hutan's own journal.
        let support = if direct {
            Some(ForkSupport::Exact)
        } else {
            parent.support
        };
        if let Some(support) = support
            && !support.allows(mode, at_head)
        {
            let agent = &spec.agent;
            let message = match support {
                ForkSupport::Head => format!(
                    "{agent} forks only at the latest turn over ACP: fork at the latest turn \
                     (`head`) or start a new chat with a summary of turns 1–{} (`summary`)",
                    ordinal
                ),
                ForkSupport::Summary => format!(
                    "{agent} cannot fork its conversation (it does not offer session/fork): only \
                     a summary of turns 1–{} can seed a new chat",
                    ordinal
                ),
                _ => format!("a {agent} chat cannot be forked"),
            };
            return Err(unsupported(message, support, at_head));
        }

        let turn_id = turn.turn_id.clone();
        let through = {
            let turn_id = turn_id.clone();
            self.read_storage(move |s| s.chat().turn_last_seq(id, &turn_id))
                .await?
        }
        .ok_or_else(|| invalid_state(format!("turn {turn_id} has no events in the journal")))?;
        let copied_events = self
            .read_storage(move |s| s.chat().events_through(id, through))
            .await?;
        if copied_events > FORK_MAX_EVENTS {
            return Err(ErrorBody::new(
                ErrorCode::Unsupported,
                format!(
                    "this chat's journal has {copied_events} events through that turn; a fork \
                     copies at most {FORK_MAX_EVENTS}"
                ),
            ));
        }

        let from = if mode == ForkMode::Summary || direct {
            None
        } else {
            let thread_id = spec.thread_id.clone().ok_or_else(|| {
                invalid_state(
                    "this chat has no conversation with its agent yet: send a message first",
                )
            })?;
            let point = self
                .fork_point(parent, &turns[index], through, mode, at_head)
                .await?;
            // A fork that works in another folder says where its parent's conversation is.
            let source_cwd = (req.where_ == ForkWhere::NewWorktree).then(|| parent.cwd.clone());
            Some(ForkFrom {
                thread_id,
                point,
                source_cwd,
            })
        };
        let seed = if mode == ForkMode::Summary {
            Some(
                self.digest_of(parent, &turns, &measured, index, through)
                    .await?,
            )
        } else {
            None
        };

        let parsed: TurnId = turn_id.parse().map_err(|_| {
            ErrorBody::new(
                ErrorCode::Internal,
                format!("a malformed turn id `{turn_id}`"),
            )
        })?;
        let origin = ForkOrigin {
            session_id: parent.id,
            turn_id: parsed,
            ordinal,
            mode,
            at_ms: registry::now_ms(),
        };
        let mut child = spec.clone();
        child.thread_id = None;
        child.fork = Some(ChatForkSpec {
            origin: origin.clone(),
            pending: Some(ForkPending {
                copy: Some(JournalCopy {
                    parent: parent.id,
                    through_seq: through,
                    through_turn: turn_id,
                    notice: notice(&parent.title, mode, ordinal, total),
                }),
                from,
            }),
            seed,
        });
        Ok(ForkPlan {
            spec: child,
            support,
            origin,
            copied_events,
        })
    }

    /// The names the parent's agent knows the end of `turn` by (`None`: the end of its
    /// conversation).
    async fn fork_point(
        &self,
        parent: &ForkParent,
        turn: &ChatTurnRow,
        through: u64,
        mode: ForkMode,
        at_head: bool,
    ) -> Result<Option<ForkPoint>, ErrorBody> {
        if mode == ForkMode::Head {
            return Ok(None);
        }
        match parent.spec.agent.as_str() {
            // Codex cuts after a turn it names, the newest one too: its thread may hold turns
            // Hutan's journal does not.
            AGENT_CODEX => Ok(Some(ForkPoint {
                provider_turn_id: Some(turn.provider_turn_id.clone().ok_or_else(|| {
                    ErrorBody::new(
                        ErrorCode::Unsupported,
                        "Codex names a turn by an id that this turn's journal has no record of \
                         (it ran before forks were possible): fork at the latest turn, or with a \
                         summary",
                    )
                })?),
                message: None,
            })),
            // The end of the conversation is the end of the turn: nothing to name.
            _ if at_head => Ok(None),
            AGENT_CLAUDE => {
                let message = self
                    .claude_message(parent.id, &turn.turn_id, through)
                    .await?;
                Ok(Some(ForkPoint {
                    provider_turn_id: None,
                    message: Some(message),
                }))
            }
            // Anything else forks at its end only; `allows` stopped a cut elsewhere when the
            // support was known, and the adapter refuses when it was not.
            _ => Ok(Some(ForkPoint {
                provider_turn_id: None,
                message: None,
            })),
        }
    }

    /// The last assistant message of `turn`, named the way Claude Code's adapter finds it: the
    /// agent's own `messageId` the journal kept, the SHA-256 of its text, and how many equal
    /// messages precede it.
    async fn claude_message(
        &self,
        session: SessionId,
        turn: &str,
        through: u64,
    ) -> Result<ForkMessage, ErrorBody> {
        let rows = self
            .read_storage(move |s| {
                s.chat()
                    .items_of_kind_through(session, "message_end", through, MESSAGE_LOOKBACK)
            })
            .await?;
        // Oldest first.
        let assistant: Vec<AgentEvent> = rows
            .iter()
            .filter_map(|row| serde_json::from_str::<AgentEvent>(&row.event_json).ok())
            .filter(|event| {
                matches!(
                    event.kind,
                    AgentEventKind::MessageEnd {
                        role: MessageRole::Assistant,
                        ..
                    }
                )
            })
            .collect();
        let target = assistant
            .iter()
            .rev()
            .find(|event| {
                event.turn_id.is_some_and(|t| t.to_string() == turn)
                    && event.provider.item_id.is_some()
            })
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::Unsupported,
                    "Claude Code's adapter finds the message to cut at by the id it gave it, and \
                     this turn's journal has none (it ran before forks were possible): fork at \
                     the latest turn, or with a summary",
                )
            })?;
        let AgentEventKind::MessageEnd { text, .. } = &target.kind else {
            return Err(ErrorBody::new(ErrorCode::Internal, "not a message"));
        };
        let occurrence = assistant
            .iter()
            .filter(|event| event.seq <= target.seq)
            .filter(|event| {
                matches!(&event.kind, AgentEventKind::MessageEnd { text: other, .. } if other == text)
            })
            .count();
        Ok(ForkMessage {
            id: target.provider.item_id.clone().unwrap_or_default(),
            fingerprint: format!("sha256:{:x}", Sha256::digest(text.as_bytes())),
            occurrence: u32::try_from(occurrence).unwrap_or(u32::MAX),
        })
    }

    /// The turns of `session` that are measurements: those whose user message is the
    /// measurement command (`hutan_core::insight::is_measure_message`, the rule the insight
    /// reducer and the chat view share). Read from the newest [`MEASURE_SCAN`] messages of the
    /// journal; a measurement older than that counts as a turn.
    pub(crate) async fn measurement_turns(
        &self,
        session: SessionId,
    ) -> Result<HashSet<String>, ErrorBody> {
        let rows = self
            .read_storage(move |s| {
                s.chat()
                    .items_of_kind_through(session, "message_end", u64::MAX, MEASURE_SCAN)
            })
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let event = serde_json::from_str::<AgentEvent>(&row.event_json).ok()?;
                match event.kind {
                    AgentEventKind::MessageEnd { role, text, .. }
                        if hutan_core::insight::is_measure_message(role, &text) =>
                    {
                        event.turn_id.map(|turn| turn.to_string())
                    }
                    _ => None,
                }
            })
            .collect())
    }

    /// The digest a summary fork hands over: the turns of the parent up to `index`, without its
    /// measurements.
    async fn digest_of(
        &self,
        parent: &ForkParent,
        turns: &[ChatTurnRow],
        measured: &HashSet<String>,
        index: usize,
        through: u64,
    ) -> Result<String, ErrorBody> {
        let id = parent.id;
        let (messages, tools) = {
            let messages = self
                .read_storage(move |s| {
                    s.chat()
                        .items_of_kind_through(id, "message_end", through, DIGEST_MESSAGES)
                })
                .await?;
            let tools = self
                .read_storage(move |s| {
                    s.chat()
                        .items_of_kind_through(id, "tool_start", through, DIGEST_TOOLS)
                })
                .await?;
            (messages, tools)
        };
        let mut kept: Vec<DigestTurn> = turns[..=index]
            .iter()
            .filter(|turn| !measured.contains(&turn.turn_id))
            .enumerate()
            .map(|(n, turn)| DigestTurn {
                ordinal: u32::try_from(n + 1).unwrap_or(u32::MAX),
                id: turn.turn_id.clone(),
                user: None,
                assistant: None,
                tools: BTreeMap::new(),
                failure: match turn.status {
                    ChatTurnStatus::Failed => Some(
                        turn.error
                            .clone()
                            .unwrap_or_else(|| "the turn failed".to_owned()),
                    ),
                    ChatTurnStatus::Cancelled => Some("the turn was interrupted".to_owned()),
                    _ => None,
                },
            })
            .collect();
        let position = |turn: Option<TurnId>, kept: &[DigestTurn]| {
            let turn = turn?.to_string();
            kept.iter().position(|t| t.id == turn)
        };
        for row in messages.iter().chain(tools.iter()) {
            let Ok(event) = serde_json::from_str::<AgentEvent>(&row.event_json) else {
                continue;
            };
            let Some(at) = position(event.turn_id, &kept) else {
                continue;
            };
            match event.kind {
                AgentEventKind::MessageEnd {
                    role: MessageRole::User,
                    text,
                    ..
                } if kept[at].user.is_none() && !text.trim().is_empty() => {
                    kept[at].user = Some(text);
                }
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    text,
                    ..
                } if !text.trim().is_empty() => kept[at].assistant = Some(text),
                AgentEventKind::ToolStart { name, .. } => {
                    *kept[at].tools.entry(name).or_default() += 1;
                }
                _ => {}
            }
        }
        Ok(digest(&parent.title, &kept))
    }

    /// Copies the parent's journal prefix into the child (once: a first start that was cut short
    /// finds the copy there) and appends the journal's own notice about the fork.
    pub(super) async fn copy_fork_journal(
        &self,
        child: SessionId,
        copy: &JournalCopy,
    ) -> Result<(), ErrorBody> {
        if self.read_storage(move |s| s.chat().last_seq(child)).await? > 0 {
            return Ok(());
        }
        let event = AgentEventDraft::new(AgentEventKind::Notice {
            level: NoticeLevel::Info,
            message: copy.notice.clone(),
        })
        .into_event(child, copy.through_seq + 1, registry::now_ms());
        let event_json = serde_json::to_string(&event)
            .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("encoding a notice: {e}")))?;
        let row = ChatItemRow {
            session_id: child,
            seq: event.seq,
            ts_ms: event.ts_ms,
            turn_id: None,
            item_id: None,
            kind: event.kind.name().to_owned(),
            event_json,
        };
        let copy = copy.clone();
        let copied = self
            .read_storage(move |s| {
                s.chat().fork_journal(
                    copy.parent,
                    child,
                    copy.through_seq,
                    &copy.through_turn,
                    vec![row],
                )
            })
            .await
            .map_err(|e| {
                ErrorBody::new(
                    e.code,
                    format!("copying the journal of the parent chat: {}", e.message),
                )
            })?;
        info!(session = %child, events = copied.events, turns = copied.turns, "journal of a fork copied");
        Ok(())
    }
}

/// One turn of the digest.
#[derive(Debug)]
struct DigestTurn {
    ordinal: u32,
    id: String,
    user: Option<String>,
    assistant: Option<String>,
    /// Tool name → calls.
    tools: BTreeMap<String, u32>,
    failure: Option<String>,
}

/// `text` cut to at most `max` characters (not bytes), marked when it was cut; line breaks
/// folded into spaces.
fn clip(text: &str, max: usize) -> String {
    let folded: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() <= max {
        return folded;
    }
    let mut cut: String = folded.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// How much of a turn the digest shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    Full,
    Short,
    Line,
}

impl DigestTurn {
    fn render(&self, detail: Detail) -> String {
        let (user, assistant) = match detail {
            Detail::Full => (800, 1500),
            Detail::Short => (240, 400),
            Detail::Line => (140, 0),
        };
        let user_text = self
            .user
            .as_deref()
            .map_or_else(|| "(no message)".to_owned(), |text| clip(text, user));
        if detail == Detail::Line {
            return format!("Turn {} — User: {user_text}\n", self.ordinal);
        }
        let mut out = format!("Turn {}\n  User: {user_text}\n", self.ordinal);
        if let Some(text) = &self.assistant {
            out.push_str(&format!("  Assistant: {}\n", clip(text, assistant)));
        }
        if !self.tools.is_empty() {
            let used: Vec<String> = self
                .tools
                .iter()
                .map(|(name, count)| format!("{name} ×{count}"))
                .collect();
            out.push_str(&format!("  Tools used: {}\n", used.join(", ")));
        }
        if let Some(failure) = &self.failure {
            out.push_str(&format!("  ({})\n", clip(failure, 200)));
        }
        out
    }
}

/// The text a summary fork hands the agent: what the kept turns said and did, within
/// [`DIGEST_BUDGET`]. The newest turns keep the most detail; older ones shrink to a line, and
/// the oldest are left out (and said so) when even that does not fit.
fn digest(title: &str, turns: &[DigestTurn]) -> String {
    let header = |omitted: usize| {
        let mut text = format!(
            "This conversation continues an earlier chat, “{title}”. Its own conversation could \
             not be handed over, so what follows is a digest of its first {} turn(s), written by \
             Hutan from the journal (not by a model). Long messages are cut and tool output is \
             left out. Treat it as background; do not redo the work it describes unless asked.\n",
            turns.len()
        );
        if omitted > 0 {
            text.push_str(&format!("The first {omitted} turn(s) are not shown.\n"));
        }
        text.push('\n');
        text
    };
    // Every turn at its shortest, the oldest dropped until those lines take half the budget:
    // the rest is what the newest turns get to show.
    let lines: Vec<String> = turns.iter().map(|t| t.render(Detail::Line)).collect();
    let mut first = 0;
    let mut used = lines.iter().map(String::len).sum::<usize>() + header(0).len();
    while used > DIGEST_BUDGET / 2 && first + 1 < turns.len() {
        used -= lines[first].len();
        first += 1;
        used = used - header(first - 1).len() + header(first).len();
    }
    // Then give detail back from the newest turn on, while the budget lasts.
    let mut chosen: Vec<(Detail, String)> = lines[first..]
        .iter()
        .cloned()
        .map(|line| (Detail::Line, line))
        .collect();
    for at in (0..chosen.len()).rev() {
        for detail in [Detail::Full, Detail::Short] {
            let rendered = turns[first + at].render(detail);
            if used - chosen[at].1.len() + rendered.len() <= DIGEST_BUDGET {
                used = used - chosen[at].1.len() + rendered.len();
                chosen[at] = (detail, rendered);
                break;
            }
        }
    }
    let mut text = header(first);
    for (_, rendered) in chosen {
        text.push_str(&rendered);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(n: u32, user: &str, assistant: &str) -> DigestTurn {
        DigestTurn {
            ordinal: n,
            id: format!("t{n}"),
            user: Some(user.to_owned()),
            assistant: Some(assistant.to_owned()),
            tools: BTreeMap::from([("bash".to_owned(), 2), ("read".to_owned(), 1)]),
            failure: None,
        }
    }

    #[test]
    fn a_short_chat_is_digested_in_full() {
        let text = digest(
            "Fix the login",
            &[
                turn(1, "Why does login fail?", "The cookie is not set."),
                turn(2, "Fix it.", "Done: set the cookie."),
            ],
        );
        assert!(
            text.contains("“Fix the login”") && text.contains("2 turn(s)"),
            "{text}"
        );
        assert!(text.contains("Turn 1\n  User: Why does login fail?\n  Assistant: The cookie is not set.\n  Tools used: bash ×2, read ×1\n"), "{text}");
        assert!(
            text.contains("Turn 2\n  User: Fix it.\n  Assistant: Done: set the cookie."),
            "{text}"
        );
        assert!(!text.contains("not shown"));
        assert!(text.contains("not by a model"));
    }

    #[test]
    fn a_long_chat_keeps_its_newest_turns_in_detail_and_stays_within_the_budget() {
        let long = "word ".repeat(2000);
        let turns: Vec<DigestTurn> = (1..=200).map(|n| turn(n, &long, &long)).collect();
        let text = digest("Big", &turns);
        assert!(text.len() <= DIGEST_BUDGET, "{} bytes", text.len());
        // The newest turn is shown with its assistant message; an old one is one line or gone.
        assert!(text.contains("Turn 200\n  User:"), "newest in detail");
        let old = text.find("Turn 150").map(|at| &text[at..at + 40]);
        assert!(
            old.is_none_or(|snippet| !snippet.contains("Assistant")),
            "{old:?}"
        );
        let tail = text.split("Turn 199").nth(1).unwrap_or_default();
        assert!(
            tail.contains("Assistant:"),
            "recent turns keep their answers"
        );
    }

    #[test]
    fn when_even_one_line_per_turn_does_not_fit_the_oldest_turns_are_left_out_and_said_so() {
        let turns: Vec<DigestTurn> = (1..=2000)
            .map(|n| turn(n, &"x".repeat(300), "answer"))
            .collect();
        let text = digest("Huge", &turns);
        assert!(text.len() <= DIGEST_BUDGET, "{} bytes", text.len());
        assert!(text.contains("turn(s) are not shown"), "{}", &text[..400]);
        assert!(
            text.contains("Turn 2000"),
            "the newest turn is always there"
        );
        assert!(!text.contains("Turn 1 —"), "the oldest is gone");
    }

    #[test]
    fn clipping_counts_characters_and_folds_line_breaks() {
        assert_eq!(clip("a\n\n b   c", 20), "a b c");
        assert_eq!(clip("ünïcödé", 4), "ünï…");
        assert_eq!(clip("ünï", 3), "ünï");
    }

    #[test]
    fn a_failed_turn_says_so() {
        let mut failed = turn(1, "Run it", "");
        failed.assistant = None;
        failed.failure = Some("rate limited".to_owned());
        let text = digest("t", &[failed]);
        assert!(text.contains("(rate limited)"), "{text}");
    }
}
