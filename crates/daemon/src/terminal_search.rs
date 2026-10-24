//! `terminal.search` (protocol 0.43, `docs/protocol.md` § Terminal search): the scan itself and
//! the registry of running searches. [`TerminalHub`](crate::terminals::TerminalHub) wires them
//! to requests.
//!
//! A search reads the terminal's text a few thousand rows at a time
//! ([`CHUNK_ROWS`]) through [`Terminal::scan_lines`]: the engine is locked only while one slice
//! is read, so output keeps flowing, and between slices the search looks at its cancel token
//! and its time budget. Rows are absolute, so a search that continues in another call
//! (`next_from`) is not disturbed by output that arrived meanwhile.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hutan_core::TerminalId;
use hutan_protocol::control::{ErrorBody, ErrorCode};
use hutan_protocol::terminal_search::{
    TERMINAL_SEARCH_LINE_MAX_CHARS, TERMINAL_SEARCH_MAX_LIMIT, TERMINAL_SEARCH_QUERY_MAX_BYTES,
    TerminalSearch, TerminalSearchMatch, TerminalSearchResult,
};
use hutan_terminal::{BlockTextError, ScanFlow, Terminal};
use regex::{Regex, RegexBuilder};
use tokio_util::sync::CancellationToken;

use crate::conn::ConnId;

/// Rows read with the terminal's engine locked.
pub const CHUNK_ROWS: usize = 4096;
/// Time one call may run before it answers with what it has (`timed_out`).
pub const BUDGET: Duration = Duration::from_secs(2);
/// Most compiled bytes of a pattern (the `regex` crate's `size_limit`).
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// How much a search may take.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    /// Rows scanned per slice (at least one line each).
    pub chunk_rows: usize,
    /// Time before the search stops and answers `timed_out`; checked between slices, so a call
    /// always scans at least one.
    pub budget: Duration,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            chunk_rows: CHUNK_ROWS,
            budget: BUDGET,
        }
    }
}

/// Why a search ended without an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// A newer search of the same connection and terminal (or a closed connection) cancelled it.
    Superseded,
    /// `block_id` names no block of the terminal.
    NoSuchBlock(u64),
    /// The alternate screen hides the primary rows a block's output is in.
    Unavailable,
}

impl SearchError {
    pub fn into_body(self, terminal: TerminalId) -> ErrorBody {
        match self {
            Self::Superseded => {
                let mut error = ErrorBody::new(
                    ErrorCode::Conflict,
                    "superseded by a newer `terminal.search`",
                );
                error.data = Some(serde_json::json!({ "reason": "superseded" }));
                error
            }
            Self::NoSuchBlock(block) => ErrorBody::new(
                ErrorCode::NotFound,
                format!("block {block} of terminal {terminal} not found"),
            ),
            Self::Unavailable => ErrorBody::new(
                ErrorCode::InvalidState,
                BlockTextError::Unavailable.to_string(),
            ),
        }
    }
}

/// A validated request: the compiled pattern and clamped numbers.
#[derive(Debug)]
pub struct Params {
    matcher: Option<Regex>,
    limit: usize,
    from: u64,
    block_id: Option<u64>,
    text: bool,
}

impl Params {
    /// `bad_request` for a `query` over 1 KiB and for a pattern that does not compile within
    /// the size limit. An empty query is valid and matches nothing.
    pub fn new(req: &TerminalSearch) -> Result<Self, ErrorBody> {
        if req.query.len() > TERMINAL_SEARCH_QUERY_MAX_BYTES {
            return Err(ErrorBody::new(
                ErrorCode::BadRequest,
                format!("the query is longer than {TERMINAL_SEARCH_QUERY_MAX_BYTES} bytes"),
            ));
        }
        let matcher = if req.query.is_empty() {
            None
        } else {
            let pattern = if req.regex {
                req.query.clone()
            } else {
                regex::escape(&req.query)
            };
            let built = RegexBuilder::new(&pattern)
                .case_insensitive(!req.case)
                .size_limit(REGEX_SIZE_LIMIT)
                .dfa_size_limit(REGEX_SIZE_LIMIT * 2)
                .build()
                .map_err(|error| {
                    ErrorBody::new(ErrorCode::BadRequest, format!("invalid pattern: {error}"))
                })?;
            Some(built)
        };
        Ok(Self {
            matcher,
            limit: usize::try_from(req.limit.clamp(1, TERMINAL_SEARCH_MAX_LIMIT)).unwrap_or(1),
            from: req.from.unwrap_or(0),
            block_id: req.block_id,
            text: req.text,
        })
    }
}

/// The matches of one logical line, at most `cap` of them.
fn line_matches(
    matcher: &Regex,
    line: &hutan_terminal::SearchLine<'_>,
    text: bool,
    cap: usize,
) -> Vec<TerminalSearchMatch> {
    let mut found: Vec<TerminalSearchMatch> = Vec::new();
    let mut shown: Option<String> = None;
    for hit in matcher.find_iter(line.text) {
        if hit.is_empty() {
            continue;
        }
        let (row, col) = line.position(hit.start());
        let (end_row, end_col) = line.end_position(hit.end());
        let line_text = text.then(|| {
            shown
                .get_or_insert_with(|| {
                    line.text
                        .chars()
                        .take(TERMINAL_SEARCH_LINE_MAX_CHARS)
                        .collect()
                })
                .clone()
        });
        found.push(TerminalSearchMatch {
            row,
            col,
            end_row,
            end_col,
            block_id: line.block_id(),
            line: line_text,
        });
        if found.len() >= cap {
            break;
        }
    }
    found
}

/// Runs the search. `after_slice` runs after every slice, with the engine unlocked (progress
/// hook; the tests use it to cancel at an exact point).
pub fn run(
    terminal: &Terminal,
    params: &Params,
    bounds: &Bounds,
    cancel: &CancellationToken,
    mut after_slice: impl FnMut(),
) -> Result<TerminalSearchResult, SearchError> {
    let started = Instant::now();
    let mut matches: Vec<TerminalSearchMatch> = Vec::new();
    let mut from = params.from;
    loop {
        if cancel.is_cancelled() {
            return Err(SearchError::Superseded);
        }
        let mut limit_hit = false;
        let progress = terminal
            .scan_lines(from, bounds.chunk_rows.max(1), params.block_id, |line| {
                let Some(matcher) = &params.matcher else {
                    return ScanFlow::Continue;
                };
                let room = params.limit - matches.len();
                let mut found = line_matches(matcher, line, params.text, params.limit + 1);
                if found.is_empty() {
                    return ScanFlow::Continue;
                }
                if found.len() <= room {
                    matches.append(&mut found);
                    return ScanFlow::Continue;
                }
                // More than fit: a line is never split between two answers, unless it alone
                // is more than an answer holds.
                limit_hit = true;
                if matches.is_empty() {
                    found.truncate(params.limit);
                    matches.append(&mut found);
                    ScanFlow::StopAfter
                } else {
                    ScanFlow::StopBefore
                }
            })
            .map_err(|error| match error {
                BlockTextError::NotFound => SearchError::NoSuchBlock(params.block_id.unwrap_or(0)),
                BlockTextError::Unavailable => SearchError::Unavailable,
            })?;
        after_slice();
        let timed_out = !limit_hit && !progress.done() && started.elapsed() >= bounds.budget;
        if limit_hit || timed_out || progress.done() {
            if cancel.is_cancelled() {
                return Err(SearchError::Superseded);
            }
            let truncated = limit_hit || timed_out;
            return Ok(TerminalSearchResult {
                matches,
                truncated,
                next_from: truncated.then_some(progress.next),
                timed_out,
                viewport_top: progress.viewport_top,
                first_row: progress.first_row,
            });
        }
        from = progress.next;
    }
}

/// Running searches: at most one per connection and terminal, a newer one cancels the older.
#[derive(Debug, Default)]
pub struct Searches {
    next: u64,
    running: HashMap<(ConnId, TerminalId), (u64, CancellationToken)>,
}

impl Searches {
    /// Registers a search; the older one of the same connection and terminal is cancelled.
    /// Returns the handle for [`end`](Self::end) and the token the search watches.
    pub fn begin(&mut self, conn: ConnId, terminal: TerminalId) -> (u64, CancellationToken) {
        self.next += 1;
        let token = CancellationToken::new();
        if let Some((_, older)) = self
            .running
            .insert((conn, terminal), (self.next, token.clone()))
        {
            older.cancel();
        }
        (self.next, token)
    }

    /// The search `handle` ended; a newer one that replaced it stays registered.
    pub fn end(&mut self, conn: ConnId, terminal: TerminalId, handle: u64) {
        if self
            .running
            .get(&(conn, terminal))
            .is_some_and(|(running, _)| *running == handle)
        {
            self.running.remove(&(conn, terminal));
        }
    }

    /// Cancels every search of a closed connection.
    pub fn cancel_connection(&mut self, conn: ConnId) {
        self.running.retain(|(owner, _), (_, token)| {
            let keep = *owner != conn;
            if !keep {
                token.cancel();
            }
            keep
        });
    }

    /// Cancels every search on a terminal that went away.
    pub fn cancel_terminal(&mut self, terminal: TerminalId) {
        self.running.retain(|(_, owner), (_, token)| {
            let keep = *owner != terminal;
            if !keep {
                token.cancel();
            }
            keep
        });
    }
}
