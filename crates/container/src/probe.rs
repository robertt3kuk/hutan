//! The seam between detection and the machine.
//!
//! Detection never touches the machine itself: it asks a [`HostProbe`]. The real one is
//! [`crate::SystemProbe`]; tests inject a fake fed with recorded output. Every method is
//! read-only by contract: look at the file system, run a *query* command, `GET` an Engine API
//! endpoint. Nothing starts, selects, pulls, builds or runs (spike § 3).

use std::path::{Path, PathBuf};

/// What `stat` says about a path (symlinks followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Missing,
    Socket,
    Directory,
    File,
    Other,
}

/// Why a probe step produced nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    /// No such program or socket path.
    #[error("not found")]
    NotFound,
    /// The socket file exists but nothing accepts connections (a stale socket).
    #[error("connection refused")]
    Refused,
    /// The step exceeded its time bound (2 s in [`crate::SystemProbe`]).
    #[error("timed out")]
    TimedOut,
    #[error("{0}")]
    Io(String),
    /// The peer answered with something that is not HTTP.
    #[error("malformed HTTP response: {0}")]
    Malformed(String),
}

/// A finished query command. A non-zero exit status is normal (`container system status`
/// exits 1 while the services are down and still prints its JSON), so it is data, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// `None` when killed by a signal.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// An Engine API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Everything detection needs from the host. Implementations bound each call (2 s); detection
/// is on demand and callers cache a result for at most 30 s (spike § 3).
pub trait HostProbe {
    /// Milliseconds since the Unix epoch (the report's `checked_at`).
    fn now_ms(&self) -> u64;
    fn home_dir(&self) -> Option<PathBuf>;
    fn env_var(&self, name: &str) -> Option<String>;
    /// The executable `name` on the login `PATH` or in the known install directories.
    fn find_executable(&self, name: &str) -> Option<PathBuf>;
    fn path_kind(&self, path: &Path) -> PathKind;
    /// Symlinks resolved; `None` when the path does not exist.
    fn canonicalize(&self, path: &Path) -> Option<PathBuf>;
    /// Entries of `dir` as full paths, sorted; empty when it does not exist.
    fn list_dir(&self, dir: &Path) -> Vec<PathBuf>;
    /// Run a query command with stdin closed.
    fn run(&self, program: &Path, args: &[&str]) -> Result<CommandOutput, ProbeError>;
    /// `GET path` over the unix socket `socket`.
    fn http_get(&self, socket: &Path, path: &str) -> Result<HttpResponse, ProbeError>;
}

/// Parse the bytes of one HTTP/1.1 response read until EOF (`Connection: close`): status line,
/// headers, then a `Content-Length`, chunked or close-delimited body.
pub fn parse_http_response(raw: &[u8]) -> Result<HttpResponse, ProbeError> {
    let malformed = |what: &str| ProbeError::Malformed(what.to_string());
    let split = find(raw, b"\r\n\r\n").ok_or_else(|| malformed("no end of headers"))?;
    let head =
        std::str::from_utf8(&raw[..split]).map_err(|_| malformed("headers are not UTF-8"))?;
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.splitn(3, ' ');
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/1.")) {
        return Err(malformed("not an HTTP/1.x status line"));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| malformed("no status code"))?;
    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| malformed("header without `:`"))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    let find_header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    let body_bytes = if find_header("transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        decode_chunked(body)?
    } else if let Some(len) = find_header("content-length") {
        let len: usize = len.parse().map_err(|_| malformed("bad Content-Length"))?;
        if body.len() < len {
            return Err(malformed("body shorter than Content-Length"));
        }
        body[..len].to_vec()
    } else {
        body.to_vec()
    };
    let body = String::from_utf8(body_bytes).map_err(|_| malformed("body is not UTF-8"))?;
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn decode_chunked(mut rest: &[u8]) -> Result<Vec<u8>, ProbeError> {
    let malformed = |what: &str| ProbeError::Malformed(format!("chunked body: {what}"));
    let mut out = Vec::new();
    loop {
        let eol = find(rest, b"\r\n").ok_or_else(|| malformed("no chunk size line"))?;
        let size_text =
            std::str::from_utf8(&rest[..eol]).map_err(|_| malformed("size is not text"))?;
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| malformed("bad chunk size"))?;
        rest = &rest[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if rest.len() < size + 2 {
            return Err(malformed("truncated chunk"));
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_chunked_and_close_delimited_bodies() {
        let r = parse_http_response(
            b"HTTP/1.1 200 OK\r\nApi-Version: 1.54\r\nContent-Length: 2\r\n\r\nOKextra",
        )
        .unwrap();
        assert_eq!((r.status, r.body.as_str()), (200, "OK"));
        assert_eq!(r.header("api-version"), Some("1.54"));

        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n";
        assert_eq!(parse_http_response(chunked).unwrap().body, "hello world");

        let close = b"HTTP/1.0 200 OK\r\nServer: x\r\n\r\nbody until EOF";
        assert_eq!(parse_http_response(close).unwrap().body, "body until EOF");
    }

    #[test]
    fn rejects_what_is_not_http() {
        for raw in [
            &b""[..],
            b"garbage",
            b"SSH-2.0-OpenSSH\r\n\r\n",
            b"HTTP/1.1 abc\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nbad header\r\n\r\n",
        ] {
            assert!(
                matches!(parse_http_response(raw), Err(ProbeError::Malformed(_))),
                "{raw:?}"
            );
        }
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort").is_err());
        assert!(
            parse_http_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel")
                .is_err()
        );
    }
}
