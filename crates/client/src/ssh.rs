//! A daemon on another machine, reached through the system `ssh` (ADR 0022, slice R-2).
//!
//! `ssh -T … <destination> <hutan> proxy` runs the bytes-only bridge `hutan proxy` on the host;
//! its stdin and stdout, which are `ssh`'s own, carry the protocol exactly as the local Unix
//! socket does. Hutan stores no password or key and opens no listener: authentication, host
//! keys, jump hosts, agents and connection sharing are the user's `~/.ssh/config` and OpenSSH.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::watch;
use tokio::time::timeout;

use crate::error::ClientError;

/// Environment variable naming the `ssh` program to run instead of `ssh` from `PATH` (a wrapper
/// that adds `-F` or `-i`, or a test double).
pub const SSH_PROGRAM_ENV: &str = "HUTAN_SSH";

/// How long the whole connect-and-handshake may take: a cold SSH connection through a jump
/// host, plus a remote daemon that is started by the proxy, takes longer than a local socket.
pub(crate) const SSH_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the end of `ssh`'s standard error is kept for.
const STDERR_TAIL: usize = 4096;
/// How much of what `ssh` printed first is kept to recognise text that is not a frame.
const HEAD: usize = 128;
/// How long a failed handshake waits for the rest of `ssh`'s standard error.
const STDERR_GRACE: Duration = Duration::from_millis(300);

/// A host reached over SSH.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHost {
    /// The profile's name, for messages.
    pub name: String,
    /// What `ssh` is given as the host: `user@host`, an `ssh_config` alias or an `ssh://` URI.
    destination: String,
    /// `hutan` on the host: a path, or a name looked up on the non-interactive `PATH` there.
    hutan: Option<String>,
    /// The `ssh` program; `None` is `$HUTAN_SSH`, else `ssh`.
    program: Option<PathBuf>,
}

impl SshHost {
    /// A host `name` at `destination`. Refuses a destination `ssh` could read as an option or
    /// that is not a single word, because the destination comes from a settings file.
    pub fn new(
        name: impl Into<String>,
        destination: impl Into<String>,
        hutan: Option<String>,
    ) -> Result<Self, ClientError> {
        let name = name.into();
        let destination = destination.into();
        if destination.is_empty()
            || destination.starts_with('-')
            || destination.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(ClientError::Ssh(format!(
                "host `{name}`: `{destination}` is not an ssh destination (user@host, a \
                 ~/.ssh/config alias or an ssh:// URI, one word, not starting with `-`)"
            )));
        }
        if let Some(path) = &hutan
            && (path.is_empty() || path.chars().any(char::is_control))
        {
            return Err(ClientError::Ssh(format!(
                "host `{name}`: the `hutan` path on the host is empty or has control characters"
            )));
        }
        Ok(Self {
            name,
            destination,
            hutan,
            program: None,
        })
    }

    /// Runs this `ssh` program instead of `$HUTAN_SSH` / `ssh`.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = Some(program.into());
        self
    }

    pub fn destination(&self) -> &str {
        &self.destination
    }

    pub fn hutan(&self) -> Option<&str> {
        self.hutan.as_deref()
    }

    /// The `ssh` program that runs.
    pub fn program(&self) -> OsString {
        self.program
            .clone()
            .map(PathBuf::into_os_string)
            .or_else(|| std::env::var_os(SSH_PROGRAM_ENV).filter(|v| !v.is_empty()))
            .unwrap_or_else(|| OsString::from("ssh"))
    }

    /// The command the host's shell runs: `hutan proxy`.
    pub fn proxy_command(&self) -> String {
        format!("{} proxy", remote_word(self.hutan.as_deref().unwrap_or("hutan")))
    }

    /// Options every connection of Hutan sets, before the user's own `ssh_config` applies for
    /// everything else: no terminal, no prompt (a password or an unknown host key fails the
    /// connection, it is never asked for), no forwarding of agent, display or ports of the
    /// user's configuration (they belong to their interactive sessions), and dead links noticed
    /// within a minute.
    const BASE_OPTIONS: [&'static str; 14] = [
        "-T",
        "-a",
        "-x",
        "-o",
        "BatchMode=yes",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "--",
    ];

    /// The arguments of `ssh` that run the proxy.
    pub fn proxy_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = Self::BASE_OPTIONS.iter().map(OsString::from).collect();
        args.push(OsString::from(&self.destination));
        args.push(OsString::from(self.proxy_command()));
        args
    }

    /// Spawns `ssh … hutan proxy` and returns its stdio as one stream, with what is needed to
    /// explain a failed handshake.
    pub(crate) fn open(&self) -> Result<(SshStream, Arc<SshDiagnostics>), ClientError> {
        let program = self.program();
        let mut command = Command::new(&program);
        command
            .args(self.proxy_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Terminal signals (Ctrl-C of a CLI) reach `hutan`, which ends `ssh` itself.
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| {
            ClientError::Ssh(format!(
                "cannot run `{}`: {e}",
                program.to_string_lossy()
            ))
        })?;
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(ClientError::Ssh("ssh started without its pipes".into()));
        };
        let diagnostics = Arc::new(SshDiagnostics::default());
        let sink = diagnostics.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 512];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.push_stderr(&chunk[..n]),
                }
            }
            sink.stderr_ended.send_replace(true);
        });
        Ok((
            SshStream {
                stdin: Some(stdin),
                stdout,
                head: diagnostics.clone(),
                _child: child,
            },
            diagnostics,
        ))
    }
}

/// One word for the host's shell: left alone when it is plain, else single-quoted; a leading
/// `~/` stays outside the quotes so the shell still expands it.
fn remote_word(path: &str) -> String {
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | '@'))
    };
    if plain(path) {
        return path.to_owned();
    }
    let (home, rest) = match path.strip_prefix("~/") {
        Some(rest) => ("~/", rest),
        None => ("", path),
    };
    if home.is_empty() || !rest.is_empty() {
        let quoted = rest.replace('\'', r"'\''");
        return format!("{home}'{quoted}'");
    }
    home.to_owned()
}

/// What `ssh` said, kept bounded.
#[derive(Debug)]
pub(crate) struct SshDiagnostics {
    head: Mutex<Vec<u8>>,
    stderr: Mutex<Vec<u8>>,
    /// `true` once `ssh`'s standard error has ended, i.e. everything it said is in `stderr`.
    stderr_ended: watch::Sender<bool>,
}

impl Default for SshDiagnostics {
    fn default() -> Self {
        Self {
            head: Mutex::default(),
            stderr: Mutex::default(),
            stderr_ended: watch::channel(false).0,
        }
    }
}

impl SshDiagnostics {
    fn push_head(&self, bytes: &[u8]) {
        let mut head = self.head.lock().unwrap_or_else(PoisonError::into_inner);
        let room = HEAD.saturating_sub(head.len());
        head.extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    fn push_stderr(&self, bytes: &[u8]) {
        let mut tail = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        tail.extend_from_slice(bytes);
        if tail.len() > STDERR_TAIL {
            let excess = tail.len() - STDERR_TAIL;
            tail.drain(..excess);
        }
    }

    /// The end of what `ssh` or the remote command wrote to standard error, as one line each.
    pub(crate) fn stderr_text(&self) -> String {
        let tail = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        let text = String::from_utf8_lossy(&tail);
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The first bytes of the host's output when they are text and not a frame: every frame of
    /// the protocol starts with a zero byte (its length is below 16 MiB), so a banner or a
    /// line a shell startup file printed starts with something else.
    fn stray_text(&self) -> Option<String> {
        let head = self.head.lock().unwrap_or_else(PoisonError::into_inner);
        let first = *head.first()?;
        if first == 0 {
            return None;
        }
        let line = String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .collect::<String>();
        Some(line)
    }

    /// `error`, said so that the user can act on it. An answer of the daemon (its own refusal)
    /// is the daemon's to say and passes as it is.
    pub(crate) async fn explain(&self, host: &SshHost, error: ClientError) -> ClientError {
        if matches!(error, ClientError::Remote(_)) {
            return error;
        }
        let name = &host.name;
        if let Some(text) = self.stray_text() {
            return ClientError::Ssh(format!(
                "ssh {name}: the host printed text before hutan's first message (`{text}`); a shell \
                 startup file that writes to standard output (.zshenv, .bashrc without an \
                 interactive guard, a banner) breaks the connection. Make non-interactive \
                 shells silent (see docs/remote.md)"
            ));
        }
        // `ssh` that just exited may not have been read to the end of its standard error yet.
        let mut ended = self.stderr_ended.subscribe();
        let _ = timeout(STDERR_GRACE, ended.wait_for(|ended| *ended)).await;
        let stderr = self.stderr_text();
        if stderr.is_empty() {
            return ClientError::Ssh(format!("ssh {name}: {error}"));
        }
        ClientError::Ssh(format!("ssh {name}: {stderr}"))
    }
}

/// The standard input and output of `ssh` as one duplex stream. Shutting the write side down
/// closes the pipe (the remote proxy sees end of input and ends); dropping the stream kills
/// `ssh` if it still runs, and tokio reaps it.
#[derive(Debug)]
pub(crate) struct SshStream {
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    head: Arc<SshDiagnostics>,
    _child: Child,
}

impl AsyncRead for SshStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.stdout).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) {
            self.head.push_head(&buf.filled()[before..]);
        }
        polled
    }
}

impl AsyncWrite for SshStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.stdin.as_mut() {
            Some(stdin) => Pin::new(stdin).poll_write(cx, data),
            None => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.stdin.as_mut() {
            Some(stdin) => Pin::new(stdin).poll_flush(cx),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Dropping the pipe is what closes it; `ChildStdin`'s own shutdown does nothing.
        self.stdin = None;
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_words_are_quoted_for_the_hosts_shell() {
        assert_eq!(remote_word("hutan"), "hutan");
        assert_eq!(remote_word("/opt/homebrew/bin/hutan"), "/opt/homebrew/bin/hutan");
        assert_eq!(remote_word("~/bin/hutan"), "~/bin/hutan");
        assert_eq!(remote_word("~/my bin/hutan"), "~/'my bin/hutan'");
        assert_eq!(remote_word("/a b/it's"), r"'/a b/it'\''s'");
        assert_eq!(remote_word("x; rm -rf ~"), "'x; rm -rf ~'");
    }

    #[test]
    fn destinations_that_ssh_would_read_as_options_are_refused() {
        for bad in ["", "-oProxyCommand=x", "a b", "host\n", "-F"] {
            assert!(SshHost::new("h", bad, None).is_err(), "{bad:?}");
        }
        for good in ["u@h", "alias", "ssh://u@127.0.0.1:2222", "10.0.0.1"] {
            assert!(SshHost::new("h", good, None).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn the_proxy_runs_without_a_terminal_a_prompt_or_forwarding() {
        let host = SshHost::new("h", "u@host", Some("~/bin/hutan".into())).unwrap();
        let args: Vec<String> = host
            .proxy_args()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-T".to_owned()));
        assert!(args.contains(&"BatchMode=yes".to_owned()));
        assert!(args.contains(&"ClearAllForwardings=yes".to_owned()));
        // The destination follows `--`, so it can never be an option; the command is last.
        let dashes = args.iter().position(|a| a == "--").unwrap();
        assert_eq!(&args[dashes + 1..], ["u@host", "~/bin/hutan proxy"]);
    }

    #[tokio::test]
    async fn text_before_the_first_frame_is_named() {
        let host = SshHost::new("box", "u@h", None).unwrap();
        let diagnostics = SshDiagnostics::default();
        diagnostics.push_head(b"Welcome to box!\nmore");
        let error = diagnostics
            .explain(&host, ClientError::Handshake("bad frame".into()))
            .await;
        let text = error.to_string();
        assert!(text.contains("Welcome to box!"), "{text}");
        assert!(text.contains("startup file"), "{text}");

        // A frame starts with a zero byte: nothing to say about its head.
        let diagnostics = SshDiagnostics::default();
        diagnostics.push_head(&[0, 0, 1, 2, 3]);
        diagnostics.push_stderr(b"Permission denied (publickey).\n");
        diagnostics.stderr_ended.send_replace(true);
        let text = diagnostics
            .explain(&host, ClientError::Disconnected)
            .await
            .to_string();
        assert_eq!(text, "ssh box: Permission denied (publickey).");
    }

    #[test]
    fn the_stderr_tail_is_bounded() {
        let diagnostics = SshDiagnostics::default();
        for _ in 0..100 {
            diagnostics.push_stderr(&[b'x'; 1000]);
        }
        assert_eq!(diagnostics.stderr_text().len(), STDERR_TAIL);
    }
}
