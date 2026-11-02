//! One verification command: a `/bin/sh -c` in its own process group, the login environment, the
//! output of both pipes in one bounded tail, a timeout, and a way to stop it that takes the whole
//! process tree (`docs/assistant.md` § Checks › Verification).

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use hutan_protocol::verify::VERIFY_TAIL_BYTES;
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The output kept while the command runs: more than the tail the run reports, so that redaction
/// sees a secret whole before the cut is made.
const CAPTURE_BYTES: usize = 64 * 1024;
/// How long a grandchild that keeps a pipe open after the command ended may keep the run.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);

/// What to run.
#[derive(Debug)]
pub struct Spec {
    pub command: String,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
}

/// How the command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    TimedOut,
    Cancelled,
    /// The command could not be started.
    StartFailed(String),
}

/// The end and the output.
#[derive(Debug)]
pub struct Outcome {
    pub ended: Ended,
    /// The last [`VERIFY_TAIL_BYTES`] of the output, redacted, without control characters.
    pub tail: String,
    /// More was written than the tail holds.
    pub truncated: bool,
}

#[derive(Debug, Default)]
struct Tail {
    bytes: Vec<u8>,
    /// Bytes thrown away from the front.
    dropped: bool,
}

impl Tail {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        // Cut at twice the capture so a stream of small chunks does not move the buffer each time.
        if self.bytes.len() > 2 * CAPTURE_BYTES {
            let cut = self.bytes.len() - CAPTURE_BYTES;
            self.bytes.drain(..cut);
            self.dropped = true;
        }
    }
}

async fn read_into<R: AsyncRead + Unpin>(mut pipe: R, into: Arc<Mutex<Tail>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => into.lock().push(&chunk[..n]),
        }
    }
}

/// Runs `spec` until it ends, `spec.timeout` passes, or one of `stop` is cancelled. A stopped
/// command is killed with everything it started.
pub async fn run(spec: Spec, stop: &[CancellationToken]) -> Outcome {
    let failed = |why: String| Outcome {
        ended: Ended::StartFailed(why),
        tail: String::new(),
        truncated: false,
    };
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(&spec.command)
        .envs(&spec.env)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed(format!("could not start the command: {error}")),
    };
    let pid = child.id();
    let tail = Arc::new(Mutex::new(Tail::default()));
    let readers: Vec<JoinHandle<()>> = [
        child
            .stdout
            .take()
            .map(|pipe| tokio::spawn(read_into(pipe, tail.clone()))),
        child
            .stderr
            .take()
            .map(|pipe| tokio::spawn(read_into(pipe, tail.clone()))),
    ]
    .into_iter()
    .flatten()
    .collect();

    let stopped = async {
        let waits: Vec<_> = stop.iter().map(CancellationToken::cancelled).collect();
        futures::future::select_all(waits.into_iter().map(Box::pin)).await;
    };
    let ended = tokio::select! {
        status = child.wait() => match status {
            Ok(status) => Ended::Exited { code: status.code(), signal: status.signal() },
            Err(error) => Ended::StartFailed(format!("could not wait for the command: {error}")),
        },
        () = tokio::time::sleep(spec.timeout) => Ended::TimedOut,
        () = stopped => Ended::Cancelled,
    };
    if matches!(ended, Ended::TimedOut | Ended::Cancelled) {
        // The command and everything it started go, then the zombie is reaped.
        if let Some(pid) = pid {
            let _ = tokio::task::spawn_blocking(move || crate::resources::kill_tree(pid)).await;
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    // What was written before the end is read; a grandchild that keeps a pipe open does not hold
    // the run.
    for mut reader in readers {
        if tokio::time::timeout(OUTPUT_GRACE, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
    let (bytes, dropped) = {
        let tail = tail.lock();
        (tail.bytes.clone(), tail.dropped)
    };
    let (tail, truncated) = finish_output(&bytes, dropped);
    Outcome {
        ended,
        tail,
        truncated,
    }
}

/// The output as a run reports it: text with line breaks and tabs but no other control
/// characters (terminal colours and cursor moves are dropped, a carriage return is a line
/// break), redacted, and cut to its last [`VERIFY_TAIL_BYTES`] — in that order, so a secret is
/// never cut into something redaction no longer recognises.
pub fn finish_output(bytes: &[u8], dropped: bool) -> (String, bool) {
    let text = clean(&String::from_utf8_lossy(bytes));
    let redacted = hutan_index::redact::redact(&text);
    let mut cut = dropped;
    let mut start = redacted.len().saturating_sub(VERIFY_TAIL_BYTES);
    if start > 0 {
        cut = true;
        while !redacted.is_char_boundary(start) {
            start += 1;
        }
    }
    (redacted[start..].to_owned(), cut)
}

/// Drops ANSI escape sequences and control characters other than `\n` and `\t`; `\r\n` is a line
/// break and a lone `\r` too.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // CSI: parameters, then a final byte in `@`..=`~`.
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST.
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            chars.next();
                            break;
                        }
                    }
                }
                // Any other two-byte escape.
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.push('\n');
                }
            }
            '\n' | '\t' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_control_characters_are_dropped_and_line_breaks_kept() {
        assert_eq!(
            clean("\u{1b}[31mFAILED\u{1b}[0m x\r\nnext\rlast\u{7}\tend\u{1b}]0;title\u{7}!"),
            "FAILED x\nnext\nlast\tend!"
        );
    }

    #[test]
    fn the_tail_is_the_last_bytes_and_says_when_it_is_cut() {
        let (whole, cut) = finish_output(b"all of it\n", false);
        assert_eq!((whole.as_str(), cut), ("all of it\n", false));
        let text = "é".repeat(VERIFY_TAIL_BYTES);
        let (tail, cut) = finish_output(text.as_bytes(), false);
        assert!(cut && tail.len() <= VERIFY_TAIL_BYTES);
        assert!(
            tail.chars().all(|c| c == 'é'),
            "cut on a character boundary"
        );
        assert!(
            finish_output(b"x", true).1,
            "bytes dropped while reading count"
        );
    }

    #[test]
    fn a_secret_is_redacted_before_the_tail_is_cut() {
        let secret = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let text = format!("{} {secret}\n", "x".repeat(VERIFY_TAIL_BYTES - 10));
        let (tail, _) = finish_output(text.as_bytes(), false);
        assert!(!tail.contains("AAAAAAAA"), "{tail}");
    }
}
