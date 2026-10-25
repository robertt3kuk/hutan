//! [`SystemProbe`]: the real [`HostProbe`], std only.
//!
//! Each call is bounded (default 2 s) and output is capped (1 MiB), so a hung or chatty runtime
//! cannot stall or flood detection. Nothing here is a mutation: `stat`, `readlink`, directory
//! listings, query commands with stdin closed, and `GET` over a unix socket. Tests never run it
//! against the machine's runtimes; they use a fake [`HostProbe`], and exercise this one only
//! against a socket and scripts they create in a temporary directory.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::probe::{
    CommandOutput, HostProbe, HttpResponse, PathKind, ProbeError, parse_http_response,
};

const MAX_OUTPUT: u64 = 1 << 20;

#[derive(Debug, Clone)]
pub struct SystemProbe {
    /// The login `PATH` (the daemon already computes it, `docs/agents.md`), searched first.
    path_dirs: Vec<PathBuf>,
    timeout: Duration,
}

impl SystemProbe {
    /// Directories searched after the login `PATH`: where the runtimes' installers put their CLI.
    const FALLBACK_DIRS: [&'static str; 2] = ["/usr/local/bin", "/opt/homebrew/bin"];

    pub fn new(path_dirs: Vec<PathBuf>) -> Self {
        Self {
            path_dirs,
            timeout: Duration::from_secs(2),
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn search_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = self.path_dirs.clone();
        dirs.extend(Self::FALLBACK_DIRS.iter().map(PathBuf::from));
        if let Some(home) = self.home_dir() {
            for sub in [".orbstack/bin", ".docker/bin", ".rd/bin"] {
                dirs.push(home.join(sub));
            }
        }
        dirs
    }
}

fn read_capped(mut source: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        // A read error keeps what arrived: partial output is still output.
        let _ = source.by_ref().take(MAX_OUTPUT).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

fn io_error(e: std::io::Error) -> ProbeError {
    match e.kind() {
        ErrorKind::NotFound => ProbeError::NotFound,
        ErrorKind::ConnectionRefused => ProbeError::Refused,
        ErrorKind::TimedOut | ErrorKind::WouldBlock => ProbeError::TimedOut,
        _ => ProbeError::Io(e.to_string()),
    }
}

impl HostProbe for SystemProbe {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.env_var("HOME")
            .filter(|h| h.starts_with('/'))
            .map(PathBuf::from)
    }

    fn env_var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    fn find_executable(&self, name: &str) -> Option<PathBuf> {
        self.search_dirs()
            .into_iter()
            .map(|dir| dir.join(name))
            .find(|candidate| {
                std::fs::metadata(candidate)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }

    fn path_kind(&self, path: &Path) -> PathKind {
        match std::fs::metadata(path) {
            Err(_) => PathKind::Missing,
            Ok(m) if m.file_type().is_socket() => PathKind::Socket,
            Ok(m) if m.is_dir() => PathKind::Directory,
            Ok(m) if m.is_file() => PathKind::File,
            Ok(_) => PathKind::Other,
        }
    }

    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        std::fs::canonicalize(path).ok()
    }

    fn list_dir(&self, dir: &Path) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(read) => read.filter_map(|e| e.ok().map(|e| e.path())).collect(),
            Err(_) => Vec::new(),
        };
        entries.sort();
        entries
    }

    fn run(&self, program: &Path, args: &[&str]) -> Result<CommandOutput, ProbeError> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(io_error)?;
        let stdout = child.stdout.take().map(read_capped);
        let stderr = child.stderr.take().map(read_capped);
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait().map_err(io_error)? {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    // Our own child: killing it is the one way to honour the bound.
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ProbeError::TimedOut);
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        // A grandchild holding the pipe open must not hold detection hostage.
        let collect = |rx: Option<mpsc::Receiver<Vec<u8>>>| {
            rx.and_then(|rx| rx.recv_timeout(Duration::from_millis(250)).ok())
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        };
        Ok(CommandOutput {
            status: status.code(),
            stdout: collect(stdout),
            stderr: collect(stderr),
        })
    }

    fn http_get(&self, socket: &Path, path: &str) -> Result<HttpResponse, ProbeError> {
        let mut stream = UnixStream::connect(socket).map_err(io_error)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(io_error)?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(io_error)?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: hutan\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).map_err(io_error)?;
        let mut raw = Vec::new();
        stream
            .take(MAX_OUTPUT)
            .read_to_end(&mut raw)
            .map_err(io_error)?;
        parse_http_response(&raw)
    }
}
