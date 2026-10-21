//! What the server tells the client without being asked, kept as bounded **state** and
//! announced as bounded **events**.
//!
//! State is authoritative: the diagnostics of each file ([`DiagnosticsStore`], at most
//! [`MAX_DIAGNOSTIC_FILES`] files of at most [`MAX_DIAGNOSTICS_PER_FILE`] entries, messages cut
//! to [`MAX_MESSAGE_BYTES`]), the active work-done progress tokens, the last [`MAX_LOG_LINES`]
//! log messages. Events only say that something changed. Every subscriber has its own queue of
//! [`EVENT_QUEUE`] events; when a subscriber does not keep up, new events for it are dropped
//! and its next [`Subscription::recv`] after the queue drained returns
//! [`ServerEvent::Lagged`]: it re-reads the state it cares about. A slow subscriber therefore
//! never holds memory beyond its queue, and never slows the server down.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ls_types::{Diagnostic, MessageType, ProgressToken, Uri};
use tokio::sync::mpsc;

use crate::server::ServerState;

/// Events waiting per subscriber.
pub const EVENT_QUEUE: usize = 256;
/// Subscribers at most.
pub const MAX_SUBSCRIBERS: usize = 32;
/// Files with diagnostics kept; a file beyond is not stored ([`DiagnosticsStore::truncated`]).
pub const MAX_DIAGNOSTIC_FILES: usize = 4096;
/// Diagnostics kept per file (the rest is dropped, [`FileDiagnostics::truncated`]).
pub const MAX_DIAGNOSTICS_PER_FILE: usize = 500;
/// Longest message of a diagnostic, log line or progress text, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024;
/// Log messages kept (`window/logMessage`, `window/showMessage`).
pub const MAX_LOG_LINES: usize = 256;
/// Work-done progress tokens tracked at once.
pub const MAX_PROGRESS_TOKENS: usize = 64;

/// Something changed; read the matching state for the details.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    /// The lifecycle state changed.
    State(ServerState),
    /// The server replaced the diagnostics of `uri` ([`crate::Server::diagnostics`]).
    Diagnostics { uri: Uri, version: Option<i32> },
    /// Work-done progress (`$/progress`).
    Progress {
        token: ProgressToken,
        phase: ProgressPhase,
        title: Option<String>,
        message: Option<String>,
        percentage: Option<u32>,
    },
    /// A log or show message from the server.
    Log(LogEntry),
    /// The server ended unexpectedly and starts again after `delay`.
    Restarting {
        attempt: u32,
        delay: Duration,
        reason: String,
    },
    /// Events for this subscriber were dropped because it did not keep up.
    Lagged(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressPhase {
    Begin,
    Report,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub level: MessageType,
    pub message: String,
}

/// The diagnostics a server published for one file (they replace the previous set).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileDiagnostics {
    pub version: Option<i32>,
    pub diagnostics: Arc<[Diagnostic]>,
    /// The server sent more than [`MAX_DIAGNOSTICS_PER_FILE`].
    pub truncated: bool,
}

/// One subscriber's queue.
#[derive(Debug)]
pub struct Subscription {
    rx: mpsc::Receiver<ServerEvent>,
    lagged: Arc<AtomicU64>,
}

impl Subscription {
    /// The next event; `None` once the server handle is gone. After dropped events, a
    /// [`ServerEvent::Lagged`] follows the events queued before them.
    pub async fn recv(&mut self) -> Option<ServerEvent> {
        if let Ok(event) = self.rx.try_recv() {
            return Some(event);
        }
        let dropped = self.lagged.swap(0, Ordering::AcqRel);
        if dropped > 0 {
            return Some(ServerEvent::Lagged(dropped));
        }
        self.rx.recv().await
    }

    /// Events waiting in this subscriber's queue (never more than [`EVENT_QUEUE`]).
    pub fn len(&self) -> usize {
        self.rx.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rx.is_empty()
    }
}

#[derive(Debug)]
struct Subscriber {
    tx: mpsc::Sender<ServerEvent>,
    lagged: Arc<AtomicU64>,
}

/// Fan-out to every subscriber without waiting for any.
#[derive(Debug, Default)]
pub(crate) struct Fanout {
    subscribers: Vec<Subscriber>,
}

impl Fanout {
    pub fn subscribe(&mut self) -> Option<Subscription> {
        self.subscribers.retain(|s| !s.tx.is_closed());
        if self.subscribers.len() >= MAX_SUBSCRIBERS {
            return None;
        }
        let (tx, rx) = mpsc::channel(EVENT_QUEUE);
        let lagged = Arc::new(AtomicU64::new(0));
        self.subscribers.push(Subscriber {
            tx,
            lagged: lagged.clone(),
        });
        Some(Subscription { rx, lagged })
    }

    pub fn send(&mut self, event: &ServerEvent) {
        self.subscribers
            .retain(|subscriber| match subscriber.tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    subscriber.lagged.fetch_add(1, Ordering::AcqRel);
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            });
    }
}

/// `text` cut to at most `max` bytes on a character boundary.
pub(crate) fn clip(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The diagnostics of every file, as last published.
#[derive(Debug, Default)]
pub struct DiagnosticsStore {
    files: HashMap<Uri, FileDiagnostics>,
    /// A file was not stored because [`MAX_DIAGNOSTIC_FILES`] were.
    truncated: bool,
}

impl DiagnosticsStore {
    /// Stores what the server published; `false` when the file did not fit.
    pub(crate) fn publish(
        &mut self,
        uri: Uri,
        version: Option<i32>,
        mut diagnostics: Vec<Diagnostic>,
    ) -> bool {
        if diagnostics.is_empty() {
            self.files.remove(&uri);
            return true;
        }
        if !self.files.contains_key(&uri) && self.files.len() >= MAX_DIAGNOSTIC_FILES {
            self.truncated = true;
            return false;
        }
        let truncated = diagnostics.len() > MAX_DIAGNOSTICS_PER_FILE;
        diagnostics.truncate(MAX_DIAGNOSTICS_PER_FILE);
        for diagnostic in &mut diagnostics {
            diagnostic.message = clip(std::mem::take(&mut diagnostic.message), MAX_MESSAGE_BYTES);
        }
        self.files.insert(
            uri,
            FileDiagnostics {
                version,
                diagnostics: diagnostics.into(),
                truncated,
            },
        );
        true
    }

    pub fn get(&self, uri: &Uri) -> Option<&FileDiagnostics> {
        self.files.get(uri)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Uri, &FileDiagnostics)> {
        self.files.iter()
    }

    /// Forgets everything (the server that published it is gone); returns the files that had
    /// diagnostics.
    pub(crate) fn clear(&mut self) -> Vec<Uri> {
        self.truncated = false;
        self.files.drain().map(|(uri, _)| uri).collect()
    }
}

/// The last log lines, oldest first.
#[derive(Debug, Default)]
pub(crate) struct LogRing {
    lines: VecDeque<LogEntry>,
}

impl LogRing {
    pub fn push(&mut self, level: MessageType, message: String) -> LogEntry {
        if self.lines.len() == MAX_LOG_LINES {
            self.lines.pop_front();
        }
        let entry = LogEntry {
            level,
            message: clip(message, MAX_MESSAGE_BYTES),
        };
        self.lines.push_back(entry.clone());
        entry
    }

    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.lines.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use ls_types::{Position, Range};

    use super::*;

    fn diagnostic(message: &str) -> Diagnostic {
        Diagnostic::new_simple(
            Range::new(Position::new(0, 0), Position::new(0, 1)),
            message.to_owned(),
        )
    }

    #[tokio::test]
    async fn a_slow_subscriber_lags_instead_of_growing() {
        let mut fanout = Fanout::default();
        let mut slow = fanout.subscribe().unwrap();
        for i in 0..(EVENT_QUEUE as u64 + 10) {
            fanout.send(&ServerEvent::Lagged(i));
            assert!(slow.len() <= EVENT_QUEUE);
        }
        for i in 0..EVENT_QUEUE as u64 {
            assert_eq!(slow.recv().await, Some(ServerEvent::Lagged(i)));
        }
        assert_eq!(slow.recv().await, Some(ServerEvent::Lagged(10)));
        // Closed subscribers are pruned; the limit holds.
        drop(slow);
        let mut kept = Vec::new();
        while let Some(subscription) = fanout.subscribe() {
            kept.push(subscription);
        }
        assert_eq!(kept.len(), MAX_SUBSCRIBERS);
    }

    #[test]
    fn the_store_is_bounded_and_replaces_per_file() {
        let mut store = DiagnosticsStore::default();
        let uri: Uri = "file:///a".parse().unwrap();
        let many = (0..MAX_DIAGNOSTICS_PER_FILE + 5)
            .map(|_| diagnostic(&"é".repeat(MAX_MESSAGE_BYTES)))
            .collect();
        assert!(store.publish(uri.clone(), Some(1), many));
        let file = store.get(&uri).unwrap();
        assert!(file.truncated);
        assert_eq!(file.diagnostics.len(), MAX_DIAGNOSTICS_PER_FILE);
        assert!(file.diagnostics[0].message.len() <= MAX_MESSAGE_BYTES);
        assert!(store.publish(uri.clone(), Some(2), vec![diagnostic("x")]));
        assert_eq!(store.get(&uri).unwrap().diagnostics.len(), 1);
        assert!(store.publish(uri.clone(), Some(3), Vec::new()));
        assert!(store.get(&uri).is_none());

        for i in 0..MAX_DIAGNOSTIC_FILES + 3 {
            let uri: Uri = format!("file:///f{i}").parse().unwrap();
            store.publish(uri, None, vec![diagnostic("x")]);
        }
        assert_eq!(store.len(), MAX_DIAGNOSTIC_FILES);
        assert!(store.truncated());
    }
}
