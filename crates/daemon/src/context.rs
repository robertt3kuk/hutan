//! The pure half of the assistant's context providers (`docs/assistant.md`): how a resolved
//! piece of context is redacted, cut to its share of the budget, headed and assembled into the
//! text a message carries. Reading terminals, files and the index is [`crate::assistant`].
//!
//! Every text goes through the history index's redactor *before* it is cut, so a secret
//! straddling the cut is gone as a whole, and the headers (which carry a command line or a
//! path) are redacted as well.

use hutan_agents::ImageBlock;
use hutan_index::redact::redact;
use hutan_protocol::assistant::{Attachment, AttachmentKind, AttachmentMeta};

/// Characters read beyond a part's share so that redaction sees a secret that straddles the cut.
pub const REDACT_SLACK: usize = 512;
/// Longest label or header description, in characters.
const MAX_LABEL_CHARS: usize = 80;

/// What a message carries besides the user's words: the assembled context and what it says
/// about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// The text appended to the user's message (headers and bodies).
    pub appendix: String,
    /// Kind, label, size and truncation per attachment, in the order they were attached.
    pub meta: Vec<AttachmentMeta>,
    /// The text attachments one by one (header and body), for agents that take them as separate
    /// resources (ACP embedded context).
    pub blocks: Vec<hutan_agents::ContextBlock>,
    /// The images (protocol 0.23): their headers are part of the appendix, their bytes go to
    /// the backend as image content.
    pub images: Vec<ImageBlock>,
}

/// Which end of a text survives a cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// The beginning (files, search results, transcripts).
    Start,
    /// The end (terminal output: the error is at the bottom).
    End,
}

/// A text cut to a character budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bounded {
    pub text: String,
    pub truncated: bool,
    /// Characters of the text before the cut.
    pub total_chars: usize,
}

/// Cuts `text` to at most `max_chars` characters, on a character boundary.
pub fn bound(text: &str, max_chars: usize, keep: Keep) -> Bounded {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return Bounded {
            text: text.to_owned(),
            truncated: false,
            total_chars,
        };
    }
    let kept = match keep {
        Keep::Start => {
            let end = text
                .char_indices()
                .nth(max_chars)
                .map_or(text.len(), |(index, _)| index);
            &text[..end]
        }
        Keep::End => {
            let start = text
                .char_indices()
                .nth(total_chars - max_chars)
                .map_or(text.len(), |(index, _)| index);
            &text[start..]
        }
    };
    Bounded {
        text: kept.to_owned(),
        truncated: true,
        total_chars,
    }
}

/// Redacts `text`, then cuts it.
pub fn redact_and_bound(text: &str, max_chars: usize, keep: Keep) -> Bounded {
    bound(&redact(text), max_chars, keep)
}

/// One character budget per attachment: the budget split evenly, at least one character.
pub fn share(budget_chars: usize, attachments: usize) -> usize {
    (budget_chars / attachments.max(1)).max(1)
}

/// One line of at most [`MAX_LABEL_CHARS`] characters: whitespace runs become one space, a cut
/// ends in `…`. The result is redacted.
pub fn one_line(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let joined = words.join(" ");
    let cut = bound(&joined, MAX_LABEL_CHARS, Keep::Start);
    let mut line = redact(&cut.text).into_owned();
    if cut.truncated {
        line.push('…');
    }
    line
}

/// A resolved attachment, ready to be assembled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub kind: AttachmentKind,
    /// The short name `attachments_meta` shows.
    pub label: String,
    /// `[shell: cargo test · exit 101 · 12 lines]`, with the truncation noted.
    pub header: String,
    /// Redacted and cut; ends without a newline.
    pub body: String,
    pub truncated: bool,
    /// An image (protocol 0.23): its bytes are never part of the text.
    pub image: Option<ImagePart>,
}

/// What a resolved image adds to a [`Part`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePart {
    pub block: ImageBlock,
    /// The file the image was read from; `None` for inline data.
    pub path: Option<String>,
    /// Hex SHA-256 of the bytes.
    pub sha256: String,
}

/// The word in front of a header's description.
fn tag(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::ShellBlock => "shell",
        AttachmentKind::ShellRecent => "shell",
        AttachmentKind::AgentTranscript => "agent",
        AttachmentKind::File => "file",
        AttachmentKind::Image => "image",
        AttachmentKind::PastedText => "pasted text",
        AttachmentKind::Search => "search",
        AttachmentKind::TurnReview => "review",
    }
}

/// ` · cut to the first 2000 of 9000 characters`; the total is left out when it is not known.
fn cut_note(keep: Keep, kept: usize, total: Option<usize>) -> String {
    let end = match keep {
        Keep::Start => "first",
        Keep::End => "last",
    };
    match total {
        Some(total) => format!(" · cut to the {end} {kept} of {total} characters"),
        None => format!(" · cut to the {end} {kept} characters"),
    }
}

impl Part {
    /// Builds the part of `kind` from `body` (already redacted and cut). `description` is the
    /// header text after the kind word; `extra_truncated` says the source itself was incomplete
    /// (the terminal lost lines, a file was only read in part): it marks the part truncated
    /// without a cut of ours.
    pub fn new(
        kind: AttachmentKind,
        label: &str,
        description: &str,
        body: Bounded,
        keep: Keep,
        total_known: bool,
        extra_truncated: bool,
    ) -> Self {
        let kept = body.text.chars().count();
        let mut header = format!("[{}: {}", tag(kind), collapse(description));
        if body.truncated {
            header.push_str(&cut_note(
                keep,
                kept,
                total_known.then_some(body.total_chars),
            ));
        } else if extra_truncated {
            header.push_str(" · incomplete");
        }
        header.push(']');
        Self {
            kind,
            label: one_line(label),
            header,
            body: body.text.trim_end_matches(['\n', '\r']).to_owned(),
            truncated: body.truncated || extra_truncated,
            image: None,
        }
    }

    /// An image: the header says what it is, the bytes ride along unredacted.
    pub fn image(label: &str, description: &str, image: ImagePart) -> Self {
        Self {
            kind: AttachmentKind::Image,
            label: one_line(label),
            header: format!("[image: {}]", collapse(description)),
            body: String::new(),
            truncated: false,
            image: Some(image),
        }
    }

    /// The part as it is written into the message.
    pub fn text(&self) -> String {
        if self.image.is_some() {
            self.header.clone()
        } else if self.body.is_empty() {
            format!("{}\n(nothing)", self.header)
        } else {
            format!("{}\n{}", self.header, self.body)
        }
    }

    pub fn meta(&self) -> AttachmentMeta {
        let mut meta = AttachmentMeta::text(
            self.kind,
            self.label.clone(),
            u64::try_from(self.body.chars().count()).unwrap_or(u64::MAX),
            self.truncated,
        );
        if let Some(image) = &self.image {
            meta.size = Some(u64::try_from(image.block.data.len()).unwrap_or(u64::MAX));
            meta.media_type = Some(image.block.media_type.clone());
            meta.path.clone_from(&image.path);
            meta.sha256 = Some(image.sha256.clone());
        }
        meta
    }
}

/// A header description on one line, redacted. The caller cut its long components (a command)
/// with [`one_line`] already.
fn collapse(text: &str) -> String {
    redact(&text.split_whitespace().collect::<Vec<_>>().join(" ")).into_owned()
}

/// The text appended to the user's message, and what it says about itself.
pub fn assemble(parts: &[Part]) -> Attached {
    let mut appendix = String::from(
        "--- attached context (read-only, redacted, bounded by the user's settings) ---\n",
    );
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            appendix.push('\n');
        }
        appendix.push_str(&part.text());
        appendix.push('\n');
    }
    appendix.push_str("--- end of attached context ---");
    Attached {
        appendix,
        meta: parts.iter().map(Part::meta).collect(),
        blocks: parts
            .iter()
            .filter(|part| part.image.is_none())
            .map(|part| hutan_agents::ContextBlock {
                label: part.label.clone(),
                text: part.text(),
            })
            .collect(),
        images: parts
            .iter()
            .filter_map(|part| part.image.as_ref().map(|image| image.block.clone()))
            .collect(),
    }
}

/// The media type of `bytes` when they start like a PNG, JPEG, GIF or WebP file.
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// `1.5 MiB`, `120 KiB`, `800 B`.
pub fn human_size(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KiB", bytes.div_ceil(KIB))
    } else {
        format!("{bytes} B")
    }
}

/// The lines of `rows` (text, soft-wrapped into the next row) joined along soft wraps, the
/// trailing empty lines dropped, and the last `n` of them as text.
pub fn last_lines<'a>(
    rows: impl IntoIterator<Item = (&'a str, bool)>,
    n: usize,
) -> (String, usize) {
    let mut lines: Vec<String> = Vec::new();
    let mut open = false;
    for (text, wrapped) in rows {
        if open {
            if let Some(last) = lines.last_mut() {
                last.push_str(text);
            }
        } else {
            lines.push(text.to_owned());
        }
        open = wrapped;
    }
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(n);
    let kept = &lines[start..];
    (kept.join("\n"), kept.len())
}

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub user: bool,
    pub text: String,
}

/// Consecutive rows of the same role are chunks of one message: joined with a line break.
pub fn merge_chunks(rows: impl IntoIterator<Item = Message>) -> Vec<Message> {
    let mut merged: Vec<Message> = Vec::new();
    for row in rows {
        match merged.last_mut() {
            Some(last) if last.user == row.user => {
                last.text.push('\n');
                last.text.push_str(&row.text);
            }
            _ => merged.push(row),
        }
    }
    merged
}

/// The last `turns` turns of `messages` (a turn starts at a user message; a conversation that
/// opens with the assistant has that answer as a turn of its own) as text, and how many turns
/// that is.
pub fn last_turns(messages: &[Message], turns: usize) -> (String, usize) {
    let mut starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(index, message)| message.user || *index == 0)
        .map(|(index, _)| index)
        .collect();
    let first = starts.len().saturating_sub(turns.max(1));
    starts.drain(..first);
    let Some(&from) = starts.first() else {
        return (String::new(), 0);
    };
    let text = messages[from..]
        .iter()
        .map(|message| {
            format!(
                "{}: {}",
                if message.user { "user" } else { "assistant" },
                message.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    (text, starts.len())
}

/// Checks an attachment's own bounds (`lines`, `last_turns`, `limit`, ranges, empty names).
/// The message of the error says what was wrong.
pub fn check_bounds(attachment: &Attachment) -> Result<(), String> {
    use hutan_protocol::assistant::{
        SEARCH_MAX_LIMIT, SHELL_RECENT_MAX_LINES, TRANSCRIPT_MAX_TURNS,
    };
    match attachment {
        Attachment::ShellBlock { .. } => Ok(()),
        Attachment::ShellRecent { lines, .. } => {
            if (1..=SHELL_RECENT_MAX_LINES).contains(lines) {
                Ok(())
            } else {
                Err(format!(
                    "`shell_recent` takes 1 to {SHELL_RECENT_MAX_LINES} lines, not {lines}"
                ))
            }
        }
        Attachment::AgentTranscript {
            session_id,
            last_turns,
        } => {
            if session_id.trim().is_empty() {
                Err("`agent_transcript` needs a session id".to_owned())
            } else if !(1..=TRANSCRIPT_MAX_TURNS).contains(last_turns) {
                Err(format!(
                    "`agent_transcript` takes 1 to {TRANSCRIPT_MAX_TURNS} turns, not {last_turns}"
                ))
            } else {
                Ok(())
            }
        }
        Attachment::File { path, range, .. } => {
            if path.trim().is_empty() {
                Err("`file` needs a path".to_owned())
            } else if range.is_some_and(|r| r.start == 0 || r.end < r.start) {
                Err(
                    "`file` range lines are 1-based and `end` must not be before `start`"
                        .to_owned(),
                )
            } else {
                Ok(())
            }
        }
        Attachment::Image {
            path,
            data_base64,
            media_type,
        } => {
            use hutan_protocol::assistant::{IMAGE_INLINE_MAX_BYTES, IMAGE_MEDIA_TYPES};
            if !IMAGE_MEDIA_TYPES.contains(&media_type.to_ascii_lowercase().as_str()) {
                return Err(format!(
                    "`image` takes PNG, JPEG, GIF or WebP ({}), not `{media_type}`",
                    IMAGE_MEDIA_TYPES.join(", ")
                ));
            }
            match (path, data_base64) {
                (Some(_), Some(_)) | (None, None) => {
                    Err("`image` needs exactly one of `path` and `data_base64`".to_owned())
                }
                (Some(path), None) if path.trim().is_empty() => {
                    Err("`image` needs a path".to_owned())
                }
                // base64 is 4 characters per 3 bytes; padding and a little slack.
                (None, Some(data)) if data.len() > IMAGE_INLINE_MAX_BYTES.div_ceil(3) * 4 + 8 => {
                    Err(format!(
                        "an inline `image` is at most {IMAGE_INLINE_MAX_BYTES} bytes: attach it by `path`"
                    ))
                }
                _ => Ok(()),
            }
        }
        Attachment::PastedText { text } => {
            use hutan_protocol::assistant::PASTED_TEXT_MAX_BYTES;
            if text.trim().is_empty() {
                Err("`pasted_text` needs some text".to_owned())
            } else if text.len() > PASTED_TEXT_MAX_BYTES {
                Err(format!(
                    "`pasted_text` is {} bytes; at most {PASTED_TEXT_MAX_BYTES} are allowed",
                    text.len()
                ))
            } else {
                Ok(())
            }
        }
        Attachment::Search { query, limit } => {
            if query.trim().is_empty() {
                Err("`search` needs a query".to_owned())
            } else if !(1..=SEARCH_MAX_LIMIT).contains(limit) {
                Err(format!(
                    "`search` takes 1 to {SEARCH_MAX_LIMIT} hits, not {limit}"
                ))
            } else {
                Ok(())
            }
        }
        // A session and a turn id: whether they exist is the review's own answer.
        Attachment::TurnReview { .. } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::assistant::LineRange;

    #[test]
    fn bound_keeps_the_requested_end_on_character_boundaries() {
        let text = "aé界b😀c";
        assert_eq!(bound(text, 100, Keep::Start).text, text);
        assert!(!bound(text, 6, Keep::End).truncated);
        let start = bound(text, 3, Keep::Start);
        assert_eq!(
            (start.text.as_str(), start.truncated, start.total_chars),
            ("aé界", true, 6)
        );
        let end = bound(text, 3, Keep::End);
        assert_eq!(
            (end.text.as_str(), end.truncated, end.total_chars),
            ("b😀c", true, 6)
        );
        assert_eq!(bound("abc", 0, Keep::Start).text, "");
        assert_eq!(bound("abc", 0, Keep::End).text, "");
    }

    #[test]
    fn redaction_happens_before_the_cut() {
        let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789ABCD";
        let text = format!("key is {secret} done");
        // A cut in the middle of the key must not leave a prefix of it behind.
        let cut = redact_and_bound(&text, 20, Keep::Start);
        assert!(!cut.text.contains("sk-abc"), "{cut:?}");
        assert!(cut.truncated);
        let tail = redact_and_bound(&text, 12, Keep::End);
        assert!(!tail.text.contains("ABCD"), "{tail:?}");
    }

    #[test]
    fn the_budget_is_split_evenly_and_never_to_zero() {
        assert_eq!(share(60_000, 3), 20_000);
        assert_eq!(share(60_000, 0), 60_000);
        assert_eq!(share(2, 5), 1);
    }

    #[test]
    fn a_header_names_the_cut_and_is_one_redacted_line() {
        let body = redact_and_bound("0123456789", 4, Keep::End);
        let part = Part::new(
            AttachmentKind::ShellBlock,
            "cargo   test",
            "cargo test --token=hunter2hunter2\nsecond line · exit 101 · 10 lines",
            body,
            Keep::End,
            true,
            false,
        );
        assert!(part.truncated);
        assert_eq!(part.body, "6789");
        assert!(!part.header.contains('\n'), "{}", part.header);
        assert!(!part.header.contains("hunter2"), "{}", part.header);
        assert!(
            part.header.starts_with("[shell: cargo test ")
                && part
                    .header
                    .ends_with(" · cut to the last 4 of 10 characters]"),
            "{}",
            part.header
        );
        assert_eq!(part.label, "cargo test");
        let meta = part.meta();
        assert_eq!((meta.chars, meta.truncated), (4, true));
    }

    #[test]
    fn a_source_that_is_incomplete_says_so_without_a_cut() {
        let part = Part::new(
            AttachmentKind::File,
            "a.rs",
            "a.rs",
            bound("abc", 10, Keep::Start),
            Keep::Start,
            false,
            true,
        );
        assert!(part.truncated);
        assert_eq!(part.header, "[file: a.rs · incomplete]");
        let unknown_total = Part::new(
            AttachmentKind::File,
            "a.rs",
            "a.rs",
            bound("abcdef", 3, Keep::Start),
            Keep::Start,
            false,
            true,
        );
        assert_eq!(
            unknown_total.header,
            "[file: a.rs · cut to the first 3 characters]"
        );
    }

    #[test]
    fn assembled_context_lists_every_part_and_its_meta() {
        let one = Part::new(
            AttachmentKind::Search,
            "\"cache\"",
            "\"cache\" · 1 hit",
            bound("1. hit", 100, Keep::Start),
            Keep::Start,
            true,
            false,
        );
        let two = Part::new(
            AttachmentKind::AgentTranscript,
            "a chat",
            "a chat · 0 turns",
            bound("", 100, Keep::Start),
            Keep::Start,
            true,
            false,
        );
        let attached = assemble(&[one, two]);
        assert!(attached.appendix.starts_with("--- attached context"));
        assert!(
            attached
                .appendix
                .ends_with("--- end of attached context ---")
        );
        assert!(
            attached
                .appendix
                .contains("[search: \"cache\" · 1 hit]\n1. hit\n")
        );
        assert!(
            attached
                .appendix
                .contains("[agent: a chat · 0 turns]\n(nothing)\n")
        );
        assert_eq!(attached.meta.len(), 2);
        assert_eq!(attached.meta[0].kind, AttachmentKind::Search);
    }

    #[test]
    fn rows_join_along_soft_wraps_and_trailing_blanks_go() {
        let rows = [
            ("$ cargo build", false),
            ("a very long line that wra", true),
            ("pped here", false),
            ("done", false),
            ("", false),
            ("   ", false),
        ];
        let (text, count) = last_lines(rows, 10);
        assert_eq!(
            text,
            "$ cargo build\na very long line that wrapped here\ndone"
        );
        assert_eq!(count, 3);
        let (text, count) = last_lines(rows, 2);
        assert_eq!(text, "a very long line that wrapped here\ndone");
        assert_eq!(count, 2);
        assert_eq!(last_lines([("", false)], 5), (String::new(), 0));
    }

    fn message(user: bool, text: &str) -> Message {
        Message {
            user,
            text: text.to_owned(),
        }
    }

    #[test]
    fn chunks_of_one_message_merge_and_turns_are_counted_from_the_end() {
        let merged = merge_chunks([
            message(true, "q1 part one"),
            message(true, "q1 part two"),
            message(false, "a1"),
            message(true, "q2"),
            message(false, "a2"),
            message(true, "q3"),
        ]);
        assert_eq!(merged.len(), 5);
        assert_eq!(merged[0].text, "q1 part one\nq1 part two");
        let (text, turns) = last_turns(&merged, 2);
        assert_eq!(turns, 2);
        assert_eq!(text, "user: q2\n\nassistant: a2\n\nuser: q3");
        let (text, turns) = last_turns(&merged, 20);
        assert_eq!(turns, 3);
        assert!(text.starts_with("user: q1 part one\nq1 part two\n\nassistant: a1"));
        assert_eq!(last_turns(&[], 3), (String::new(), 0));
        // A conversation that opens with an answer keeps it as a turn.
        let (text, turns) = last_turns(&[message(false, "hello"), message(true, "hi")], 5);
        assert_eq!((text.as_str(), turns), ("assistant: hello\n\nuser: hi", 2));
    }

    #[test]
    fn attachment_bounds_are_checked() {
        let ok = |a: Attachment| check_bounds(&a).is_ok();
        let terminal_id = hutan_core::TerminalId(1);
        assert!(ok(Attachment::ShellRecent {
            terminal_id,
            lines: 500
        }));
        assert!(!ok(Attachment::ShellRecent {
            terminal_id,
            lines: 501
        }));
        assert!(!ok(Attachment::ShellRecent {
            terminal_id,
            lines: 0
        }));
        assert!(ok(Attachment::AgentTranscript {
            session_id: "s".into(),
            last_turns: 20
        }));
        assert!(!ok(Attachment::AgentTranscript {
            session_id: "s".into(),
            last_turns: 21
        }));
        assert!(!ok(Attachment::AgentTranscript {
            session_id: " ".into(),
            last_turns: 1
        }));
        assert!(ok(Attachment::Search {
            query: "x".into(),
            limit: 10
        }));
        assert!(!ok(Attachment::Search {
            query: "x".into(),
            limit: 11
        }));
        assert!(!ok(Attachment::Search {
            query: "  ".into(),
            limit: 1
        }));
        let file = |range| Attachment::File {
            path: "a".into(),
            range,
            picked: false,
        };
        assert!(ok(file(None)));
        assert!(ok(file(Some(LineRange { start: 3, end: 3 }))));
        assert!(!ok(file(Some(LineRange { start: 0, end: 3 }))));
        assert!(!ok(file(Some(LineRange { start: 4, end: 3 }))));
    }
}
