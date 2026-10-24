//! Find in terminal output (protocol 0.43, capability `terminal.search`): the daemon searches the
//! text of a terminal's scrollback and screen and answers with positions in absolute rows
//! (`docs/protocol.md` § Terminal search).

use hutan_core::TerminalId;
use serde::{Deserialize, Serialize};

/// Most matches one `terminal.search` answers, whatever `limit` asks for.
pub const TERMINAL_SEARCH_MAX_LIMIT: u32 = 1_000;
/// Longest `query` in bytes.
pub const TERMINAL_SEARCH_QUERY_MAX_BYTES: usize = 1_024;
/// Longest `line` of a match, in characters; longer lines are cut (positions stay exact).
pub const TERMINAL_SEARCH_LINE_MAX_CHARS: usize = 256;

fn is_false(value: &bool) -> bool {
    !*value
}

/// `terminal.search`: the matches of `query` in the text of a terminal, oldest first.
///
/// The text is searched per logical line: rows joined along soft wraps, trailing blanks removed,
/// so a match may continue onto the next row of a wrapped line but never crosses a hard line
/// break. Matching is case-insensitive unless `case` is set; `query` is literal text unless
/// `regex` is set (the `regex` crate's syntax, size-limited; an invalid or too large pattern is
/// `bad_request`). An empty `query` matches nothing.
///
/// A newer `terminal.search` of the same connection for the same terminal cancels this one,
/// which then answers `conflict` with `data.reason = "superseded"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSearch {
    pub terminal_id: TerminalId,
    /// At most [`TERMINAL_SEARCH_QUERY_MAX_BYTES`] bytes.
    pub query: String,
    /// Case-sensitive matching.
    #[serde(default, skip_serializing_if = "is_false")]
    pub case: bool,
    /// `query` is a regular expression.
    #[serde(default, skip_serializing_if = "is_false")]
    pub regex: bool,
    /// At most this many matches (clamped to 1 and [`TERMINAL_SEARCH_MAX_LIMIT`]).
    pub limit: u32,
    /// Start at this absolute row (the first row of a line; a line that began above it is
    /// searched from here on). Absent: the oldest retained row. Page with the answer's
    /// `next_from`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<u64>,
    /// Only the output of this block (`output_start..end_line`; a running block reaches the
    /// cursor's row). Unknown block → `not_found`; the alternate screen shows → `invalid_state`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_id: Option<u64>,
    /// Fill each match's `line` (off: positions only, for clients that draw highlights).
    #[serde(default, skip_serializing_if = "is_false")]
    pub text: bool,
}

/// One match of `terminal.search`.
///
/// Rows are absolute, as in `terminal.blocks`: row 0 is the first row the terminal ever scrolled
/// into its history, numbers only grow and do not change when older rows are evicted, and the
/// first row of the viewport has the number `viewport_top`. Columns are cells (a wide character
/// takes two).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSearchMatch {
    /// Absolute row where the match starts.
    pub row: u64,
    /// Cell column where it starts.
    pub col: u16,
    /// Absolute row where it ends: after `row` when the match continues onto the next row of a
    /// wrapped line.
    pub end_row: u64,
    /// Cell column after its last character on `end_row` (exclusive).
    pub end_col: u16,
    /// The block (prompt, command or output) whose rows hold the match start, when the shell
    /// reports blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_id: Option<u64>,
    /// The logical line that holds the match (trailing blanks removed), cut at
    /// [`TERMINAL_SEARCH_LINE_MAX_CHARS`] characters; present when the request asked for `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
}

/// Result of `terminal.search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSearchResult {
    /// Oldest first.
    pub matches: Vec<TerminalSearchMatch>,
    /// More text is left to search than this answer covers: `limit` was reached, or the time
    /// bound passed (`timed_out`). Ask again with `from = next_from`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    /// The absolute row to continue from; present when `truncated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_from: Option<u64>,
    /// The search stopped at its time bound (about two seconds) before the end.
    #[serde(default, skip_serializing_if = "is_false")]
    pub timed_out: bool,
    /// Absolute row of the viewport's first row when the search ended (as `terminal.blocks`).
    pub viewport_top: u64,
    /// The oldest absolute row the terminal still holds when the search ended.
    pub first_row: u64,
}
