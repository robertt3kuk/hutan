//! `SystemProbe` against things the test creates itself: a unix socket server and scripts in a
//! temporary directory. It never looks at the machine's real runtimes.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use hutan_container::{HostProbe, PathKind, ProbeError, SystemProbe};

use crate::common::fake::fixtures as fx;

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Serve one connection: report the request head, then answer with `reply` (or, with `None`,
/// hold the connection open without answering until `release` fires).
fn serve_once(
    listener: UnixListener,
    reply: Option<Vec<u8>>,
    release: mpsc::Receiver<()>,
) -> (mpsc::Receiver<String>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).unwrap() == 0 {
                break;
            }
            head.push(byte[0]);
        }
        tx.send(String::from_utf8_lossy(&head).into_owned())
            .unwrap();
        match reply {
            Some(bytes) => stream.write_all(&bytes).unwrap(),
            None => {
                let _ = release.recv();
            }
        }
    });
    (rx, handle)
}

#[test]
fn http_get_speaks_http_1_1_over_the_unix_socket_and_parses_the_recorded_ping() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("docker.sock");
    let (_keep, release) = mpsc::channel();
    let raw = fx::DOCKER_DESKTOP.ping.replace('\n', "\r\n").into_bytes();
    let (request, server) = serve_once(UnixListener::bind(&socket).unwrap(), Some(raw), release);

    let response = SystemProbe::new(vec![])
        .http_get(&socket, "/_ping")
        .unwrap();
    server.join().unwrap();
    assert_eq!((response.status, response.body.as_str()), (200, "OK"));
    assert_eq!(response.header("server"), Some("Docker/29.3.1 (linux)"));

    let head = request.recv().unwrap();
    assert!(head.starts_with("GET /_ping HTTP/1.1\r\n"), "{head:?}");
    assert!(head.contains("\r\nConnection: close\r\n"), "{head:?}");
}

#[test]
fn http_get_decodes_chunked_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("e.sock");
    let (_keep, release) = mpsc::channel();
    let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n7\r\n{\"a\":1,\r\n6\r\n\"b\":2}\r\n0\r\n\r\n".to_vec();
    let (_request, server) = serve_once(UnixListener::bind(&socket).unwrap(), Some(raw), release);
    let response = SystemProbe::new(vec![]).http_get(&socket, "/info").unwrap();
    server.join().unwrap();
    assert_eq!(response.body, r#"{"a":1,"b":2}"#);
}

#[test]
fn http_get_tells_stale_missing_silent_and_foreign_peers_apart() {
    let dir = tempfile::tempdir().unwrap();
    let probe = SystemProbe::new(vec![]).with_timeout(Duration::from_millis(100));

    // No file at all.
    assert_eq!(
        probe.http_get(&dir.path().join("none.sock"), "/_ping"),
        Err(ProbeError::NotFound)
    );

    // A socket file left behind by a process that is gone.
    let stale = dir.path().join("stale.sock");
    drop(UnixListener::bind(&stale).unwrap());
    assert_eq!(probe.http_get(&stale, "/_ping"), Err(ProbeError::Refused));

    // A listener that never answers: the call returns at its time bound.
    let silent = dir.path().join("silent.sock");
    let (release_tx, release) = mpsc::channel();
    let (_request, server) = serve_once(UnixListener::bind(&silent).unwrap(), None, release);
    assert_eq!(probe.http_get(&silent, "/_ping"), Err(ProbeError::TimedOut));
    release_tx.send(()).unwrap();
    server.join().unwrap();

    // A peer that is not HTTP.
    let foreign = dir.path().join("ssh.sock");
    let (_keep, release) = mpsc::channel();
    let (_request, server) = serve_once(
        UnixListener::bind(&foreign).unwrap(),
        Some(b"SSH-2.0-OpenSSH_9.9\r\n\r\n".to_vec()),
        release,
    );
    assert!(matches!(
        probe.http_get(&foreign, "/_ping"),
        Err(ProbeError::Malformed(_))
    ));
    server.join().unwrap();
}

#[test]
fn run_returns_output_and_non_zero_status_as_data() {
    let dir = tempfile::tempdir().unwrap();
    let status = script(
        dir.path(),
        "status",
        r#"printf '%s' '{"status":"not running"}'; echo oops >&2; exit 1"#,
    );
    let probe = SystemProbe::new(vec![]);
    let out = probe.run(&status, &["system", "status"]).unwrap();
    assert_eq!(out.status, Some(1));
    assert_eq!(out.stdout, r#"{"status":"not running"}"#);
    assert_eq!(out.stderr, "oops\n");

    // Arguments arrive one by one and stdin is closed.
    let echo = script(
        dir.path(),
        "echo",
        r#"for a in "$@"; do printf '[%s]' "$a"; done; cat | wc -c | tr -d ' '"#,
    );
    let out = probe.run(&echo, &["a b", "--format", "json"]).unwrap();
    assert_eq!(out.stdout, "[a b][--format][json]0\n");

    assert_eq!(
        probe.run(&dir.path().join("missing"), &[]),
        Err(ProbeError::NotFound)
    );
}

#[test]
fn run_kills_a_command_that_exceeds_its_bound() {
    let dir = tempfile::tempdir().unwrap();
    let hang = script(dir.path(), "hang", "exec sleep 30");
    let probe = SystemProbe::new(vec![]).with_timeout(Duration::from_millis(150));
    assert_eq!(probe.run(&hang, &[]), Err(ProbeError::TimedOut));
}

#[test]
fn run_does_not_wait_for_a_grandchild_that_keeps_the_pipe_open() {
    let dir = tempfile::tempdir().unwrap();
    // The shell exits at once; `sleep` inherits stdout and keeps the pipe open.
    let leaky = script(dir.path(), "leaky", "printf done; sleep 30 &");
    let probe = SystemProbe::new(vec![]);
    let out = probe.run(&leaky, &[]).unwrap();
    assert_eq!((out.status, out.stdout.as_str()), (Some(0), "done"));
}

#[test]
fn find_executable_prefers_the_login_path_and_skips_non_executables() {
    let login = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let wanted = script(other.path(), "container", "true");
    std::fs::write(login.path().join("container"), "not executable").unwrap();
    std::fs::create_dir(login.path().join("docker")).unwrap();

    let probe = SystemProbe::new(vec![login.path().to_path_buf(), other.path().to_path_buf()]);
    assert_eq!(probe.find_executable("container"), Some(wanted));
    assert_eq!(
        probe.find_executable("docker"),
        None,
        "a directory is not an executable"
    );
    assert_eq!(probe.find_executable("definitely-not-installed-xyz"), None);
}

#[test]
fn file_system_queries_follow_symlinks_and_report_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let socket = root.join("real.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    let link = root.join("link.sock");
    std::os::unix::fs::symlink(&socket, &link).unwrap();
    std::fs::write(root.join("file"), "x").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();

    let probe = SystemProbe::new(vec![]);
    assert_eq!(probe.path_kind(&socket), PathKind::Socket);
    assert_eq!(probe.path_kind(&link), PathKind::Socket);
    assert_eq!(probe.path_kind(&root.join("file")), PathKind::File);
    assert_eq!(probe.path_kind(&root.join("sub")), PathKind::Directory);
    assert_eq!(probe.path_kind(&root.join("nope")), PathKind::Missing);
    assert_eq!(probe.canonicalize(&link), Some(socket.clone()));
    assert_eq!(probe.canonicalize(&root.join("nope")), None);

    let names: Vec<_> = probe
        .list_dir(&root)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["file", "link.sock", "real.sock", "sub"]);
    assert!(probe.list_dir(&root.join("nope")).is_empty());
    assert!(probe.now_ms() > 1_700_000_000_000);
}
