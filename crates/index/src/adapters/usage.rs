//! What a transcript line says about an agent's *insight*: tokens, context, cost, tools, files
//! and the turns they belong to (INSIGHT-C, `docs/integrations.md` § Insight).
//!
//! The same adapters that turn a line into index rows turn it into [`UsageEvent`]s
//! ([`Adapter::usage_line`](super::Adapter::usage_line)), so Hutan has one reader per transcript
//! format. Events carry numbers, names, paths and times — never the text of a message, a tool's
//! output or a diff. Like the rest of an adapter this is pure: no file, no clock; what survives
//! between two lines is the caller's [`UsageState`].

use std::collections::VecDeque;

use serde_json::Value;

/// A file path longer than this many bytes is dropped (the insight keeps `MAX_PATH_BYTES = 260`).
pub const MAX_PATH_BYTES: usize = 1024;
/// A tool name longer than this many bytes is cut.
pub const MAX_NAME_BYTES: usize = 64;
/// Files one line can report (a patch of a hundred files reports the first of them).
pub const MAX_FILES_PER_LINE: usize = 64;
/// Tool calls waiting for their result.
pub const MAX_PENDING_TOOLS: usize = 64;
/// Assistant messages remembered, to count a message once however many lines it was split
/// into.
pub const MAX_REMEMBERED_MESSAGES: usize = 16;

/// Token counters of one model call; a counter the transcript does not give is `None`, never
/// zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Claude Code: input that was not served from the cache. Codex: all input, cached included.
    pub input: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
    pub total: Option<u64>,
}

impl Counts {
    /// Whether any counter is above zero.
    pub fn any(&self) -> bool {
        [
            self.input,
            self.cache_read,
            self.cache_write,
            self.output,
            self.reasoning,
            self.total,
        ]
        .into_iter()
        .any(|n| n.is_some_and(|n| n > 0))
    }

    /// `self − before` for each counter both have; a counter that went down (a restarted
    /// count) is taken as it is now.
    pub fn grown_from(&self, before: &Counts) -> Counts {
        let diff = |now: Option<u64>, then: Option<u64>| match (now, then) {
            (Some(now), Some(then)) if now >= then => Some(now - then),
            (now, _) => now,
        };
        Counts {
            input: diff(self.input, before.input),
            cache_read: diff(self.cache_read, before.cache_read),
            cache_write: diff(self.cache_write, before.cache_write),
            output: diff(self.output, before.output),
            reasoning: diff(self.reasoning, before.reasoning),
            total: diff(self.total, before.total),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Add,
    Update,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
    Failed,
}

/// One thing a transcript line says. `ts_ms` is the line's own time when it has one.
#[derive(Debug, Clone, PartialEq)]
pub enum UsageEvent {
    /// Who the conversation is: its id, folder and model, as far as the line says.
    Identity {
        session_id: Option<String>,
        cwd: Option<String>,
        model: Option<String>,
    },
    TurnStart {
        ts_ms: Option<u64>,
    },
    TurnEnd {
        ts_ms: Option<u64>,
        outcome: Outcome,
        duration_ms: Option<u64>,
    },
    /// What one model call used (a delta: a call split over several lines is counted once).
    Tokens {
        ts_ms: Option<u64>,
        counts: Counts,
    },
    /// The context now.
    Context {
        ts_ms: Option<u64>,
        used: u64,
        window: Option<u64>,
        estimated: bool,
    },
    /// The context by category, as the agent printed it for `/context`.
    Breakdown {
        ts_ms: Option<u64>,
        rows: Vec<BreakdownLine>,
    },
    /// The conversation's cost so far, in US dollars (cumulative).
    Cost {
        ts_ms: Option<u64>,
        usd: f64,
    },
    /// Milliseconds spent waiting for the model so far (cumulative).
    ApiTime {
        ts_ms: Option<u64>,
        ms: u64,
    },
    Tool {
        ts_ms: Option<u64>,
        name: String,
        failed: bool,
        ms: Option<u64>,
    },
    File {
        ts_ms: Option<u64>,
        path: String,
        kind: FileKind,
    },
}

/// One category of a `/context` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakdownLine {
    pub name: String,
    pub tokens: u64,
    /// `used`, `buffer`, `free`, `deferred` as the agent says; empty when it does not.
    pub kind: String,
}

/// A tool call that has not ended (Claude Code writes the call and its result on two lines).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTool {
    pub id: String,
    pub name: String,
    pub started_ms: Option<u64>,
    pub files: Vec<(String, FileKind)>,
}

/// What survives between two lines of one transcript. A new transcript (or a rewritten one)
/// starts with `UsageState::default()`.
#[derive(Debug, Clone, Default)]
pub struct UsageState {
    pub(crate) session_id: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) model: Option<String>,
    /// The model's window, once a line said it (`/context`, Codex's `token_count`).
    pub(crate) window: Option<u64>,
    pub(crate) turn_open: bool,
    pub(crate) turn_started_ms: Option<u64>,
    /// Codex says where its turns begin and end: its `user_message` is not an edge then.
    pub(crate) explicit_turns: bool,
    pub(crate) last_ts: Option<u64>,
    /// Claude Code: the counts of the newest assistant messages by message id.
    pub(crate) messages: VecDeque<(String, Counts)>,
    pub(crate) pending: VecDeque<PendingTool>,
    /// Codex: the thread's cumulative counts at the last `token_count`.
    pub(crate) previous_total: Option<Counts>,
}

impl UsageState {
    /// The time of the newest line that had one.
    pub fn last_ts(&self) -> Option<u64> {
        self.last_ts
    }

    /// Whether a turn is open (started and not ended).
    pub fn turn_open(&self) -> bool {
        self.turn_open
    }

    pub(crate) fn note_ts(&mut self, ts: Option<u64>) {
        if let Some(ts) = ts {
            self.last_ts = Some(self.last_ts.map_or(ts, |last| last.max(ts)));
        }
    }

    /// Opens a turn at `ts`, ending the one that is still open at the last time seen.
    pub(crate) fn start_turn(&mut self, ts: Option<u64>, out: &mut Vec<UsageEvent>) {
        if self.turn_open {
            let at = self.last_ts;
            self.end_turn(at, Outcome::Completed, None, out);
        }
        self.turn_open = true;
        self.turn_started_ms = ts.or(self.last_ts);
        out.push(UsageEvent::TurnStart { ts_ms: ts });
    }

    /// Ends the open turn (nothing is said when none is open). A turn without an explicit
    /// duration is as long as the two edges' times say.
    pub(crate) fn end_turn(
        &mut self,
        ts: Option<u64>,
        outcome: Outcome,
        duration_ms: Option<u64>,
        out: &mut Vec<UsageEvent>,
    ) {
        if !self.turn_open {
            return;
        }
        self.turn_open = false;
        let duration_ms = duration_ms.or_else(|| match (self.turn_started_ms, ts) {
            (Some(start), Some(end)) => end.checked_sub(start),
            _ => None,
        });
        out.push(UsageEvent::TurnEnd {
            ts_ms: ts,
            outcome,
            duration_ms,
        });
    }

    /// Reports the identity when a line says something new about it.
    pub(crate) fn identity(
        &mut self,
        session_id: Option<&str>,
        cwd: Option<&str>,
        model: Option<&str>,
        out: &mut Vec<UsageEvent>,
    ) {
        let new = |known: &Option<String>, line: Option<&str>| {
            line.filter(|line| !line.is_empty() && known.as_deref() != Some(*line))
                .map(str::to_owned)
        };
        let session_id = new(&self.session_id, session_id);
        let cwd = new(&self.cwd, cwd);
        let model = new(&self.model, model);
        if session_id.is_none() && cwd.is_none() && model.is_none() {
            return;
        }
        if model.is_some() {
            // The window belongs to the model that was asked.
            self.window = None;
        }
        if let Some(id) = &session_id {
            self.session_id = Some(id.clone());
        }
        if let Some(dir) = &cwd {
            self.cwd = Some(dir.clone());
        }
        if let Some(name) = &model {
            self.model = Some(name.clone());
        }
        out.push(UsageEvent::Identity {
            session_id,
            cwd,
            model,
        });
    }

    pub(crate) fn push_pending(&mut self, tool: PendingTool) {
        if self.pending.len() >= MAX_PENDING_TOOLS {
            self.pending.pop_front();
        }
        self.pending.push_back(tool);
    }

    pub(crate) fn take_pending(&mut self, id: &str) -> Option<PendingTool> {
        let at = self.pending.iter().position(|tool| tool.id == id)?;
        self.pending.remove(at)
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A path the insight may name: not empty, not control characters, not absurdly long.
pub(crate) fn plain_path(path: &str) -> Option<String> {
    (!path.is_empty() && path.len() <= MAX_PATH_BYTES && !path.chars().any(char::is_control))
        .then(|| path.to_owned())
}

/// A non-negative whole number in `value`.
pub(crate) fn number(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map(|n| n as u64)
    })
}
