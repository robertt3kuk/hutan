//! Reading a transcript from where the last read stopped (INSIGHT-C).
//!
//! A [`TailFile`] is one agent's transcript being followed: bounded reads from the last byte
//! offset, whole lines only (a line the agent is still writing waits), the file noticed when it
//! was replaced or rewritten, and a file too long to read from its first byte followed from its
//! last [`MAX_REPLAY_BYTES`]. It owns what the history adapter remembers between two lines
//! ([`UsageState`]). Blocking: callers run [`TailFile::step`] on a blocking thread.
//!
//! Bounds (`docs/integrations.md` § Insight): [`READ_BYTES`] at a time and at most
//! [`CATCH_UP_BYTES`] in one look, lines over [`MAX_LINE_BYTES`] are skipped, and nothing is read
//! when the file did not grow.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use hutan_index::Source;
use hutan_index::adapters::usage::{UsageEvent, UsageState};
use hutan_index::adapters::{Adapter, adapter_for};

use crate::transcript_tail::Route;

/// Bytes read at a time.
pub const READ_BYTES: usize = 256 * 1024;
/// Bytes read in one look (in chunks of [`READ_BYTES`]): the first look at a transcript takes all
/// of it (it is at most [`MAX_REPLAY_BYTES`]), so the numbers a request gets are the whole
/// conversation's; later looks take what was appended.
pub const CATCH_UP_BYTES: usize = 16 * 1024 * 1024;
/// A transcript longer than this is followed from its last this many bytes: what came before
/// is not counted (the insight says so).
pub const MAX_REPLAY_BYTES: u64 = 16 * 1024 * 1024;
/// A line longer than this is skipped, as the history index skips it.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// Bytes in front of the offset that are read again to notice a rewritten file.
const GUARD_BYTES: usize = 64;

/// One transcript being followed.
#[derive(Debug)]
pub struct TailFile {
    pub path: PathBuf,
    /// The agent's id of the conversation, when known.
    pub key: Option<String>,
    pub route: Route,
    source: Source,
    offset: u64,
    started: bool,
    /// `(device, inode)` of the file as it was read.
    identity: Option<(u64, u64)>,
    /// The end of the part of the file consumed, to notice a rewrite in place.
    guard: Vec<u8>,
    /// The start of a line whose end has not been read yet.
    carry: Vec<u8>,
    /// Inside a line that is not read: the rest of it up to its end is dropped.
    skipping: bool,
    usage: UsageState,
    /// The transcript was followed from the middle: older turns are not counted.
    pub cut: bool,
    /// Lines that were not valid JSON, or too long.
    pub malformed: u64,
}

/// What one [`TailFile::step`] read.
#[derive(Debug, Default)]
pub struct Step {
    pub events: Vec<UsageEvent>,
    /// The file was replaced or rewritten: what was read before is not the same conversation,
    /// and the events are those of the new content from its first line.
    pub rotated: bool,
    /// More was left to read than the step's budget.
    pub more: bool,
    pub bytes: u64,
}

impl TailFile {
    pub fn new(path: PathBuf, key: Option<String>, route: Route, source: Source) -> Self {
        Self {
            path,
            key,
            route,
            source,
            offset: 0,
            started: false,
            identity: None,
            guard: Vec::new(),
            carry: Vec::new(),
            skipping: false,
            usage: UsageState::default(),
            cut: false,
            malformed: 0,
        }
    }

    /// The byte offset the next read starts at.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    fn restart(&mut self) {
        self.offset = 0;
        self.started = false;
        self.guard.clear();
        self.carry.clear();
        self.skipping = false;
        self.usage = UsageState::default();
        self.cut = false;
    }

    /// Reads what was appended since the last step, at most `budget` bytes.
    ///
    /// # Errors
    /// The file cannot be opened or read (`NotFound` while the agent has not written it yet).
    pub fn step(&mut self, budget: usize) -> io::Result<Step> {
        let Some(adapter) = adapter_for(self.source) else {
            return Ok(Step::default());
        };
        let mut file = File::open(&self.path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::other("not a regular file"));
        }
        let identity = (meta.dev(), meta.ino());
        let len = meta.len();
        let mut step = Step::default();
        let replaced = self.identity.is_some_and(|known| known != identity)
            || len < self.offset
            || (self.offset > 0 && !self.guard_holds(&mut file)?);
        if replaced {
            self.restart();
            step.rotated = true;
        }
        self.identity = Some(identity);
        if !self.started {
            self.started = true;
            if len > MAX_REPLAY_BYTES {
                // Start inside the file: the first line is cut, and is skipped.
                self.offset = len - MAX_REPLAY_BYTES;
                self.skipping = true;
                self.cut = true;
            }
        }
        if self.offset >= len {
            return Ok(step);
        }
        let mut buffer =
            vec![0u8; READ_BYTES.min(usize::try_from(len - self.offset).unwrap_or(READ_BYTES))];
        while (step.bytes as usize) < budget && self.offset < len {
            file.seek(SeekFrom::Start(self.offset))?;
            let want = buffer
                .len()
                .min(usize::try_from(len - self.offset).unwrap_or(buffer.len()));
            let read = file.read(&mut buffer[..want])?;
            if read == 0 {
                break;
            }
            self.offset += read as u64;
            step.bytes += read as u64;
            self.consume(&buffer[..read], adapter.as_ref(), &mut step.events);
        }
        step.more = self.offset < len;
        Ok(step)
    }

    /// Whether the bytes in front of the offset are the ones that were read there.
    fn guard_holds(&self, file: &mut File) -> io::Result<bool> {
        if self.guard.is_empty() {
            return Ok(true);
        }
        let at = self.offset.saturating_sub(self.guard.len() as u64);
        let mut now = vec![0u8; self.guard.len()];
        file.seek(SeekFrom::Start(at))?;
        match file.read_exact(&mut now) {
            Ok(()) => Ok(now == self.guard),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn consume(&mut self, chunk: &[u8], adapter: &dyn Adapter, events: &mut Vec<UsageEvent>) {
        let keep = chunk.len().min(GUARD_BYTES);
        self.guard.extend_from_slice(&chunk[chunk.len() - keep..]);
        if self.guard.len() > GUARD_BYTES {
            let extra = self.guard.len() - GUARD_BYTES;
            self.guard.drain(..extra);
        }
        let mut rest = chunk;
        while let Some(at) = rest.iter().position(|byte| *byte == b'\n') {
            let (line, after) = rest.split_at(at);
            rest = &after[1..];
            if std::mem::take(&mut self.skipping) {
                self.carry.clear();
                continue;
            }
            if self.carry.is_empty() {
                self.line(line, adapter, events);
            } else if self.carry.len() + line.len() > MAX_LINE_BYTES {
                self.malformed += 1;
            } else {
                self.carry.extend_from_slice(line);
                let whole = std::mem::take(&mut self.carry);
                self.line(&whole, adapter, events);
            }
            self.carry.clear();
        }
        if self.skipping {
            return;
        }
        if self.carry.len() + rest.len() > MAX_LINE_BYTES {
            self.carry.clear();
            self.skipping = true;
            self.malformed += 1;
        } else {
            self.carry.extend_from_slice(rest);
        }
    }

    fn line(&mut self, line: &[u8], adapter: &dyn Adapter, events: &mut Vec<UsageEvent>) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let Ok(text) = std::str::from_utf8(line) else {
            self.malformed += 1;
            return;
        };
        if !adapter.usage_line(text, &mut self.usage, events) {
            self.malformed += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use super::*;

    fn tail(path: &std::path::Path) -> TailFile {
        TailFile::new(path.to_owned(), None, Route::HookPath, Source::Codex)
    }

    fn append(path: &std::path::Path, text: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    fn turn(ts: &str) -> String {
        format!(r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"task_started"}}}}"#)
    }

    #[test]
    fn a_file_is_read_once_then_only_what_was_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        append(&path, &format!("{}\n", turn("2026-11-03T10:00:00.000Z")));
        let mut file = tail(&path);
        let first = file.step(CATCH_UP_BYTES).unwrap();
        assert_eq!(first.events.len(), 1);
        assert!(!first.rotated && !first.more);
        let offset = file.offset();
        assert_eq!(offset, first.bytes);
        // Nothing appended: nothing read, no event.
        let none = file.step(CATCH_UP_BYTES).unwrap();
        assert_eq!((none.bytes, none.events.len()), (0, 0));
        append(&path, &format!("{}\n", turn("2026-11-03T10:00:05.000Z")));
        let more = file.step(CATCH_UP_BYTES).unwrap();
        assert_eq!(more.events.len(), 2, "the open turn ends, the next begins");
        assert_eq!(more.bytes, file.offset() - offset);
    }

    #[test]
    fn a_line_the_agent_is_still_writing_waits_for_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let line = turn("2026-11-03T10:00:00.000Z");
        let (head, tail_part) = line.split_at(20);
        append(&path, head);
        let mut file = tail(&path);
        assert!(file.step(CATCH_UP_BYTES).unwrap().events.is_empty());
        append(&path, tail_part);
        assert!(
            file.step(CATCH_UP_BYTES).unwrap().events.is_empty(),
            "no newline yet"
        );
        append(&path, "\n");
        assert_eq!(file.step(CATCH_UP_BYTES).unwrap().events.len(), 1);
        assert_eq!(file.malformed, 0);
    }

    #[test]
    fn the_budget_bounds_a_step_and_the_rest_comes_next() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let mut text = String::new();
        for _ in 0..4_000 {
            text.push_str(&turn("2026-11-03T10:00:00.000Z"));
            text.push('\n');
        }
        append(&path, &text);
        let mut file = tail(&path);
        let first = file.step(1).unwrap();
        assert!(first.more, "a budget of one byte reads one chunk");
        assert!(first.bytes <= READ_BYTES as u64);
        let mut events = first.events.len();
        while file
            .step(READ_BYTES)
            .map(|s| {
                events += s.events.len();
                s.more
            })
            .unwrap()
        {}
        assert_eq!(
            events,
            4_000 + 3_999,
            "every start but the first ends the turn before"
        );
        assert_eq!(file.offset(), text.len() as u64);
    }

    #[test]
    fn a_replaced_or_rewritten_file_is_read_again_from_its_first_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let line = format!("{}\n", turn("2026-11-03T10:00:00.000Z"));
        append(&path, &line.repeat(3));
        let mut file = tail(&path);
        assert_eq!(file.step(CATCH_UP_BYTES).unwrap().events.len(), 5);
        // Rewritten in place with other content of the same length: the bytes in front of the
        // offset are not the ones that were read.
        let other = format!("{}\n", turn("2026-11-03T10:00:09.000Z")).repeat(3);
        assert_eq!(other.len(), line.len() * 3);
        fs::write(&path, &other).unwrap();
        let again = file.step(CATCH_UP_BYTES).unwrap();
        assert!(again.rotated);
        assert_eq!(again.events.len(), 5);
        // Shorter than what was read: truncated.
        fs::write(&path, &line).unwrap();
        let shorter = file.step(CATCH_UP_BYTES).unwrap();
        assert!(shorter.rotated);
        assert_eq!(shorter.events.len(), 1);
        // Replaced by a new file (another inode), even a longer one.
        fs::remove_file(&path).unwrap();
        append(&path, &line.repeat(5));
        let replaced = file.step(CATCH_UP_BYTES).unwrap();
        assert!(replaced.rotated);
        assert_eq!(replaced.events.len(), 9);
    }

    #[test]
    fn a_file_that_does_not_exist_yet_is_not_found_and_garbage_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let mut file = tail(&path);
        assert_eq!(
            file.step(CATCH_UP_BYTES).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        append(&path, "not json\n\n{\"a\":1}\n");
        let step = file.step(CATCH_UP_BYTES).unwrap();
        assert!(step.events.is_empty());
        assert_eq!(
            file.malformed, 1,
            "an empty line is nothing, an object is not malformed"
        );
    }

    #[test]
    fn a_line_over_the_limit_is_skipped_to_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        let mut big = String::from("{\"x\":\"");
        big.push_str(&"a".repeat(MAX_LINE_BYTES + 10));
        big.push_str("\"}\n");
        append(&path, &big);
        append(&path, &format!("{}\n", turn("2026-11-03T10:00:00.000Z")));
        let mut file = tail(&path);
        let mut events = 0;
        loop {
            let step = file.step(CATCH_UP_BYTES).unwrap();
            events += step.events.len();
            if !step.more {
                break;
            }
        }
        assert_eq!(events, 1, "the line after the long one is read");
        assert_eq!(file.malformed, 1);
    }
}
