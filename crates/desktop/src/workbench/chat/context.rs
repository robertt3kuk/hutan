//! Explicit context for assistant messages (M16-B) and the rich input of every chat (protocol
//! 0.23, [`super::rich`]): the chips the user attaches to the next message, the `@shell` /
//! `@omp` / `@agent` / `@file` / `@search` mentions that offer them, and how a journaled user
//! message with attachments is split for display.
//!
//! Nothing here reads anything: a chip only *describes* a source and its requested bounds, and
//! becomes a structured [`Attachment`] that the daemon resolves, redacts and bounds when the
//! message is sent (`docs/assistant.md`). Only a chip's thumbnail touches GPUI.

use std::ops::Range;

use hutan_core::TerminalId;
use hutan_protocol::assistant::{
    AGENT_ASSISTANT, Attachment, AttachmentKind, AttachmentMeta, LineRange, SEARCH_MAX_LIMIT,
    SHELL_RECENT_MAX_LINES, TRANSCRIPT_MAX_TURNS,
};

/// Lines a `@shell` recent-output attachment asks for unless the user says otherwise.
pub const DEFAULT_LINES: u32 = 50;
/// Turns an agent transcript attachment asks for unless the user says otherwise.
pub const DEFAULT_TURNS: u32 = 5;
/// Hits a search attachment asks for unless the user says otherwise.
pub const DEFAULT_HITS: u32 = 5;
/// Longest part of a command shown in a chip, in characters.
const COMMAND_CHARS: usize = 40;

/// What the daemon appends to a message that carries attachments (`context::assemble`).
const APPENDIX_MARKER: &str = "--- attached context (read-only";

/// What the daemon opens a block of Mawas's memory with (`crate::memory::BLOCK_MARKER`).
const MEMORY_MARKER: &str = "--- Hutan memory";

/// What the daemon opens the line about the project's AGENTS.md with
/// (`crate::project_instructions::BLOCK_MARKER`).
const PROJECT_MARKER: &str = "--- Hutan project instructions";

/// Whether `agent` (`chat.agent`) is a gateway chat — a provider's chat API, no tools, no
/// approvals: a direct chat or an assistant. Both switch models and report provider errors.
pub fn is_gateway_chat(agent: &str) -> bool {
    agent == "direct" || agent == AGENT_ASSISTANT
}

/// Whether `agent` (`chat.agent`) is an ACP harness chat (OMP, Claude Code): the harness runs
/// it, offers its models and may switch them between turns.
pub fn is_harness_chat(agent: &str) -> bool {
    agent == "omp" || agent == "claude"
}

/// One attachment of the draft: what is attached, where from, and the bounds asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextChip {
    pub attachment: Attachment,
    /// Where it comes from: a terminal, a file path, a conversation, a query.
    pub source: String,
    /// The bounds requested: `last 50 lines`, `lines 1-40`, `last 5 turns`, `5 hits`.
    pub bounds: String,
    /// The daemon refused the message because of this kind of attachment, or the session cannot
    /// take it (the reason): the chip is drawn as refused and holds the message back.
    pub refused: Option<String>,
    /// A picture of an image chip (protocol 0.23).
    pub thumb: Option<Thumb>,
    /// A folded paste is open (its text is shown under the chips).
    pub expanded: bool,
}

/// What an image chip shows as its thumbnail.
#[derive(Debug, Clone)]
pub enum Thumb {
    /// An image file the user dropped or picked.
    Path(std::path::PathBuf),
    /// A clipboard image.
    Image(std::sync::Arc<gpui::Image>),
}

impl PartialEq for Thumb {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Path(a), Self::Path(b)) => a == b,
            (Self::Image(a), Self::Image(b)) => a.id() == b.id(),
            _ => false,
        }
    }
}

impl Eq for Thumb {}

fn shorten(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let cut: String = line.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

impl ContextChip {
    pub(super) fn new(attachment: Attachment, source: String, bounds: String) -> Self {
        Self {
            attachment,
            source,
            bounds,
            refused: None,
            thumb: None,
            expanded: false,
        }
    }

    /// The last `lines` lines of a terminal's screen and scrollback.
    pub fn recent(terminal_title: &str, terminal: TerminalId, lines: u32) -> Self {
        Self::new(
            Attachment::ShellRecent {
                terminal_id: terminal,
                lines,
            },
            terminal_title.to_owned(),
            format!("last {lines} {}", plural(lines, "line")),
        )
    }

    /// The output of one block of a terminal.
    pub fn block(
        terminal_title: &str,
        terminal: TerminalId,
        block: u64,
        command: Option<&str>,
        exit_code: Option<i32>,
    ) -> Self {
        let command = command
            .map(|c| shorten(c, COMMAND_CHARS))
            .filter(|c| !c.is_empty());
        let source = match command {
            Some(command) => format!("{terminal_title} · $ {command}"),
            None => terminal_title.to_owned(),
        };
        let bounds = match exit_code {
            Some(code) => format!("block #{block} · exit {code}"),
            None => format!("block #{block}"),
        };
        Self::new(
            Attachment::ShellBlock {
                terminal_id: terminal,
                block_id: block,
            },
            source,
            bounds,
        )
    }

    /// The last `turns` turns of a conversation: a Hutan chat, or an agent's session id.
    pub fn transcript(source: &str, session_id: &str, turns: u32) -> Self {
        Self::new(
            Attachment::AgentTranscript {
                session_id: session_id.to_owned(),
                last_turns: turns,
            },
            source.to_owned(),
            format!("last {turns} {}", plural(turns, "turn")),
        )
    }

    /// A file under the workspace roots, whole or some lines of it.
    pub fn file(path: &str, range: Option<LineRange>) -> Self {
        let bounds = match range {
            Some(range) if range.start == range.end => format!("line {}", range.start),
            Some(range) => format!("lines {}-{}", range.start, range.end),
            None => "whole file".to_owned(),
        };
        Self::new(
            Attachment::File {
                path: path.to_owned(),
                range,
                picked: false,
            },
            path.to_owned(),
            bounds,
        )
    }

    /// A whole file the user chose in the Files tree: read by its absolute `path` wherever it
    /// is (it was picked), named `shown` (its path relative to the tree's root) on the chip.
    pub fn picked_file(shown: &str, path: &str) -> Self {
        Self::new(
            Attachment::File {
                path: path.to_owned(),
                range: None,
                picked: true,
            },
            shown.to_owned(),
            "whole file".to_owned(),
        )
    }

    /// The best hits of a history search.
    pub fn search(query: &str, hits: u32) -> Self {
        Self::new(
            Attachment::Search {
                query: query.to_owned(),
                limit: hits,
            },
            format!("“{}”", shorten(query, COMMAND_CHARS)),
            format!("{hits} {}", plural(hits, "hit")),
        )
    }

    pub fn kind(&self) -> AttachmentKind {
        self.attachment.kind()
    }

    /// What the chip says before the message is sent: `file · src/lib.rs · lines 1-40`; a
    /// folded paste says `Pasted · 312 lines`.
    pub fn label(&self) -> String {
        if self.kind() == AttachmentKind::PastedText {
            return format!("{} · {}", self.source, self.bounds);
        }
        format!(
            "{} · {} · {}",
            kind_label(self.kind()),
            self.source,
            self.bounds
        )
    }

    /// Marks the chip as one the daemon or the session refuses: it is drawn as refused and
    /// holds the message back until it is removed.
    #[must_use]
    pub fn refused(mut self, reason: impl Into<String>) -> Self {
        self.refused = Some(reason.into());
        self
    }
}

fn plural(count: u32, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

/// The word a chip and the attach form use for a kind.
pub fn kind_label(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::ShellBlock => "shell block",
        AttachmentKind::ShellRecent => "shell recent",
        AttachmentKind::AgentTranscript => "agent",
        AttachmentKind::File => "file",
        AttachmentKind::Image => "image",
        AttachmentKind::PastedText => "pasted text",
        AttachmentKind::Search => "search",
    }
}

/// What the transcript's chip of an attachment says: kind, label, and what the daemon kept
/// (characters of text, the size of an image).
pub fn meta_text(meta: &AttachmentMeta) -> String {
    match meta.kind {
        AttachmentKind::Image => {
            let mut text = format!("image · {}", meta.label);
            if let Some(size) = meta.size {
                text.push_str(&format!(" · {}", super::rich::format_size(size)));
            }
            text
        }
        kind => {
            let mut text = format!(
                "{} · {} · {} chars",
                kind_label(kind),
                meta.label,
                meta.chars
            );
            if meta.truncated {
                text.push_str(" · cut to the budget");
            }
            text
        }
    }
}

/// The `daemon.toml` setting (the last key) that gates a kind, as the daemon names it in
/// `assistant.context.<setting>`.
pub fn gating_setting(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::ShellBlock | AttachmentKind::ShellRecent => "shell",
        AttachmentKind::AgentTranscript => "agents",
        AttachmentKind::File | AttachmentKind::Image | AttachmentKind::PastedText => "files",
        AttachmentKind::Search => "search",
    }
}

/// Whether the refusal naming `setting` (`assistant.context.files`, `error.data.setting`) is
/// about `kind`.
pub fn refused_by(setting: &str, kind: AttachmentKind) -> bool {
    setting
        .rsplit('.')
        .next()
        .is_some_and(|last| last == gating_setting(kind))
}

// ---- mentions ----------------------------------------------------------------------------

/// What an `@` mention in the composer offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    /// `@shell`: recent output or a finished block of a terminal.
    Shell,
    /// `@omp`: a transcript of an OMP session (reported or indexed).
    Omp,
    /// `@agent`: a transcript of any coding agent session or Hutan chat.
    Agent,
    /// `@file path[:first-last]`.
    File,
    /// `@search words`.
    Search,
}

impl MentionKind {
    pub const ALL: [MentionKind; 5] = [
        MentionKind::Shell,
        MentionKind::Omp,
        MentionKind::Agent,
        MentionKind::File,
        MentionKind::Search,
    ];

    /// The text after the `@`.
    pub fn token(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Omp => "omp",
            Self::Agent => "agent",
            Self::File => "file",
            Self::Search => "search",
        }
    }

    fn of(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }
}

/// A mention at the caret: `@file src/lib.rs:1-40`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub kind: MentionKind,
    /// What follows the keyword on the line, up to the caret, trimmed.
    pub arg: String,
    /// The mention in the draft, from its `@` to the caret.
    pub range: Range<usize>,
}

/// The mention that ends at `cursor` on its line, if any: an `@` that starts a word, one of the
/// five keywords, then the rest of the line. The last mention of the line wins.
pub fn find_mention(text: &str, cursor: usize) -> Option<Mention> {
    let before = text.get(..cursor)?;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = &before[line_start..];
    let mut found = None;
    for (at, _) in line.match_indices('@') {
        let starts_word = line[..at]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        if !starts_word {
            continue;
        }
        let rest = &line[at + 1..];
        let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let Some(kind) = MentionKind::of(&rest[..token_end]) else {
            continue;
        };
        found = Some(Mention {
            kind,
            arg: rest[token_end..].trim().to_owned(),
            range: line_start + at..cursor,
        });
    }
    found
}

/// `path` and an optional line range from `src/lib.rs`, `src/lib.rs:12`, `src/lib.rs:1-40`.
pub fn parse_file_arg(arg: &str) -> Result<(String, Option<LineRange>), String> {
    let unquote = |path: &str| {
        path.trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_owned()
    };
    let arg = arg.trim();
    if unquote(arg).is_empty() {
        return Err("name a file under the workspace, e.g. src/lib.rs:1-40".to_owned());
    }
    let Some((path, lines)) = arg.rsplit_once(':') else {
        return Ok((unquote(arg), None));
    };
    let parsed = match lines.split_once('-') {
        Some((a, b)) => a.parse::<u32>().ok().zip(b.parse::<u32>().ok()),
        None => lines.parse::<u32>().ok().map(|n| (n, n)),
    };
    let Some((start, end)) = parsed else {
        // A colon that does not introduce lines belongs to the path.
        return Ok((unquote(arg), None));
    };
    let path = unquote(path);
    if path.is_empty() {
        return Err("name a file before the line range".to_owned());
    }
    if start == 0 || end < start {
        return Err("lines are 1-based and the last must not be before the first".to_owned());
    }
    Ok((path, Some(LineRange { start, end })))
}

/// A count from `text` (empty = `default`) within `1..=max`.
pub fn parse_count(text: &str, default: u32, max: u32, what: &str) -> Result<u32, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(default);
    }
    match text.parse::<u32>() {
        Ok(n) if (1..=max).contains(&n) => Ok(n),
        _ => Err(format!("{what}: 1 to {max}")),
    }
}

/// Lines of recent output asked for by `text`.
pub fn parse_lines(text: &str) -> Result<u32, String> {
    parse_count(text, DEFAULT_LINES, SHELL_RECENT_MAX_LINES, "lines")
}

/// Turns of a transcript asked for.
pub fn clamp_turns(turns: u32) -> u32 {
    turns.clamp(1, TRANSCRIPT_MAX_TURNS)
}

/// Hits of a search asked for.
pub fn clamp_hits(hits: u32) -> u32 {
    hits.clamp(1, SEARCH_MAX_LIMIT)
}

// ---- journaled messages ------------------------------------------------------------------

/// A user message as the journal has it, split into what the user typed and the context the
/// daemon appended (`None` when the message has none): attached context, and Hutan's own
/// additions to a Mawas turn (the memory the user approved, `--- Hutan memory …`), which the
/// user did not type.
pub fn split_context(message: &str) -> (&str, Option<&str>) {
    let at = [APPENDIX_MARKER, MEMORY_MARKER, PROJECT_MARKER]
        .iter()
        .filter_map(|marker| message.find(&format!("\n\n{marker}")))
        .min();
    match at {
        Some(at) => (&message[..at], Some(message[at + 2..].trim_end())),
        None => (message, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_hutan_added_to_a_turn_is_not_what_the_user_typed() {
        let message = "why?\n\n--- Hutan memory update (added by Hutan to this turn; the user did not type this) ---\nAdded:\n- fact · global: x\n--- end of Hutan memory update ---";
        let (typed, context) = split_context(message);
        assert_eq!(typed, "why?");
        assert!(context.unwrap().starts_with("--- Hutan memory update"));
        // Attached context and memory together: both are the daemon's, from the first marker on.
        let both =
            "why?\n\n--- Hutan memory ---\nm\n\n--- attached context (read-only, redacted) ---\nc";
        assert_eq!(split_context(both).0, "why?");
        assert_eq!(split_context("just words"), ("just words", None));
        // The line about the project's AGENTS.md is Hutan's too, with or without memory before it.
        let project = "why?\n\n--- Hutan project instructions (this workspace's AGENTS.md) ---\nl\n--- end of Hutan project instructions ---";
        assert_eq!(split_context(project).0, "why?");
        assert!(
            split_context(project)
                .1
                .unwrap()
                .starts_with("--- Hutan project instructions")
        );
    }

    #[test]
    fn chips_say_kind_source_and_bounds() {
        let terminal = TerminalId(7);
        assert_eq!(
            ContextChip::recent("zsh", terminal, 50).label(),
            "shell recent · zsh · last 50 lines"
        );
        assert_eq!(
            ContextChip::recent("zsh", terminal, 1).bounds,
            "last 1 line"
        );
        assert_eq!(
            ContextChip::block("build", terminal, 7, Some("cargo test\nsecond"), Some(101)).label(),
            "shell block · build · $ cargo test · block #7 · exit 101"
        );
        assert_eq!(
            ContextChip::block("build", terminal, 8, None, None).label(),
            "shell block · build · block #8"
        );
        assert_eq!(
            ContextChip::transcript("Fix build · omp", "0f4c7d52", 5).label(),
            "agent · Fix build · omp · last 5 turns"
        );
        assert_eq!(
            ContextChip::file("src/lib.rs", Some(LineRange { start: 1, end: 40 })).label(),
            "file · src/lib.rs · lines 1-40"
        );
        assert_eq!(
            ContextChip::file("a.rs", Some(LineRange { start: 3, end: 3 })).bounds,
            "line 3"
        );
        assert_eq!(ContextChip::file("a.rs", None).bounds, "whole file");
        assert_eq!(
            ContextChip::search("quokka stall", 5).label(),
            "search · “quokka stall” · 5 hits"
        );
    }

    #[test]
    fn a_long_command_is_cut_in_the_chip_but_not_in_the_attachment() {
        let terminal = TerminalId(7);
        let command = "x".repeat(200);
        let chip = ContextChip::block("t", terminal, 1, Some(&command), None);
        assert!(chip.source.chars().count() < 60, "{}", chip.source);
        assert!(chip.source.ends_with('…'));
        assert_eq!(
            chip.attachment,
            Attachment::ShellBlock {
                terminal_id: terminal,
                block_id: 1
            }
        );
    }

    #[test]
    fn a_mention_offers_its_source_with_the_rest_of_the_line() {
        let text = "why does this fail @file src/lib.rs:1-40";
        let mention = find_mention(text, text.len()).unwrap();
        assert_eq!(mention.kind, MentionKind::File);
        assert_eq!(mention.arg, "src/lib.rs:1-40");
        assert_eq!(&text[mention.range.clone()], "@file src/lib.rs:1-40");

        let bare = find_mention("@shell", 6).unwrap();
        assert_eq!((bare.kind, bare.arg.as_str()), (MentionKind::Shell, ""));
        assert_eq!(
            find_mention("@omp scheduler", 14).unwrap().kind,
            MentionKind::Omp
        );
        assert_eq!(find_mention("@agent", 6).unwrap().kind, MentionKind::Agent);
        let search = find_mention("see @search quokka stall", 24).unwrap();
        assert_eq!(search.kind, MentionKind::Search);
        assert_eq!(search.arg, "quokka stall");
    }

    #[test]
    fn only_a_word_starting_at_sign_with_a_known_keyword_is_a_mention() {
        assert_eq!(find_mention("mail me@file now", 16), None);
        assert_eq!(find_mention("@unknown thing", 14), None);
        assert_eq!(find_mention("@shellfish", 10), None);
        assert_eq!(find_mention("plain text", 10), None);
        // Another line's mention does not count, nor text after the caret.
        assert_eq!(find_mention("@shell\nnext line", 16), None);
        assert_eq!(find_mention("@shell tail", 3), None);
        // The last mention of the line wins.
        let text = "@search a @file b.rs";
        let mention = find_mention(text, text.len()).unwrap();
        assert_eq!(
            (mention.kind, mention.arg.as_str()),
            (MentionKind::File, "b.rs")
        );
        assert_eq!(
            find_mention("é @file é.rs", "é @file é.rs".len())
                .unwrap()
                .arg,
            "é.rs"
        );
    }

    #[test]
    fn file_arguments_carry_an_optional_line_range() {
        assert_eq!(
            parse_file_arg("src/lib.rs"),
            Ok(("src/lib.rs".to_owned(), None))
        );
        assert_eq!(
            parse_file_arg(" \"a b.rs\":3-9 "),
            Ok(("a b.rs".to_owned(), Some(LineRange { start: 3, end: 9 })))
        );
        assert_eq!(
            parse_file_arg("x.rs:7"),
            Ok(("x.rs".to_owned(), Some(LineRange { start: 7, end: 7 })))
        );
        // A colon that is not a range stays in the path.
        assert_eq!(parse_file_arg("a:b.rs"), Ok(("a:b.rs".to_owned(), None)));
        assert!(parse_file_arg("").is_err());
        assert!(parse_file_arg("x.rs:0-3").is_err());
        assert!(parse_file_arg("x.rs:9-3").is_err());
        assert!(parse_file_arg(":3-4").is_err());
    }

    #[test]
    fn counts_stay_within_the_daemons_bounds() {
        assert_eq!(parse_lines(""), Ok(DEFAULT_LINES));
        assert_eq!(parse_lines(" 500 "), Ok(500));
        assert!(parse_lines("501").is_err());
        assert!(parse_lines("0").is_err());
        assert!(parse_lines("many").is_err());
        assert_eq!(clamp_turns(0), 1);
        assert_eq!(clamp_turns(99), TRANSCRIPT_MAX_TURNS);
        assert_eq!(clamp_hits(99), SEARCH_MAX_LIMIT);
    }

    #[test]
    fn a_refusal_names_the_kinds_it_is_about() {
        assert!(refused_by("assistant.context.files", AttachmentKind::File));
        assert!(refused_by(
            "assistant.global_context.shell",
            AttachmentKind::ShellBlock
        ));
        assert!(refused_by(
            "assistant.workspaces.hutan.agents",
            AttachmentKind::AgentTranscript
        ));
        assert!(!refused_by(
            "assistant.context.files",
            AttachmentKind::Search
        ));
    }

    #[test]
    fn a_journaled_message_splits_into_text_and_context() {
        let message = "why?\n\n--- attached context (read-only, redacted, bounded by the user's settings) ---\n[file: a.rs · 1 lines]\nfn a() {}\n--- end of attached context ---";
        let (text, context) = split_context(message);
        assert_eq!(text, "why?");
        let context = context.unwrap();
        assert!(context.starts_with("--- attached context"));
        assert!(context.ends_with("--- end of attached context ---"));
        assert_eq!(split_context("plain question"), ("plain question", None));
    }
}
