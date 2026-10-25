//! `shutdown` (and dropping the last clone) joins the connection's thread.
//!
//! Lives in its own test binary so no other test's connections add `hutan-client*` threads to
//! this process while it counts them.

#![allow(clippy::unwrap_used, clippy::expect_used)]
// Listing the process's threads by name needs the OS thread APIs.
#![allow(unsafe_code)]

use crate::common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::*;
use hutan_client::{ConnectOptions, Connection, ConnectionState};
use hutan_protocol::messages::{ClientInfo, ClientKind};

const PREFIX: &str = "hutan-client";

#[cfg(target_os = "macos")]
fn thread_names() -> Vec<String> {
    use std::ffi::CStr;
    use std::os::raw::{c_int, c_void};

    // `proc_info.h`: flavor listing thread handles; PROC_PIDTHREADINFO takes one as its arg.
    const PROC_PIDLISTTHREADS: c_int = 6;
    let pid = c_int::try_from(std::process::id()).unwrap();
    let mut handles = vec![0u64; 4096];
    let buffer_size = c_int::try_from(handles.len() * size_of::<u64>()).unwrap();
    // SAFETY: the buffer is valid for `buffer_size` bytes and the kernel writes at most that.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDLISTTHREADS,
            0,
            handles.as_mut_ptr().cast::<c_void>(),
            buffer_size,
        )
    };
    assert!(written > 0, "proc_pidinfo(PROC_PIDLISTTHREADS) failed");
    let count = usize::try_from(written).unwrap() / size_of::<u64>();
    let info_size = c_int::try_from(size_of::<libc::proc_threadinfo>()).unwrap();
    handles[..count]
        .iter()
        .filter_map(|handle| {
            // SAFETY: `proc_threadinfo` is plain data; zeroed is a valid value.
            let mut info: libc::proc_threadinfo = unsafe { std::mem::zeroed() };
            // SAFETY: `info` is valid for `info_size` bytes; the kernel fills at most that.
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    *handle,
                    std::ptr::from_mut(&mut info).cast::<c_void>(),
                    info_size,
                )
            };
            // A thread can end between listing and inspecting it.
            (got == info_size).then(|| {
                // SAFETY: `pth_name` is NUL-terminated within its array.
                unsafe { CStr::from_ptr(info.pth_name.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn thread_names() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|entry| std::fs::read_to_string(entry.unwrap().path().join("comm")).ok())
        .map(|name| name.trim_end().to_owned())
        .collect()
}

fn client_threads() -> usize {
    thread_names()
        .iter()
        .filter(|name| name.starts_with(PREFIX))
        .count()
}

/// The thread list of the OS can lag a moment behind `JoinHandle::join` (the kernel reaps the
/// thread asynchronously), so counts are polled until they settle on `expected`.
fn assert_client_threads(expected: usize, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let count = client_threads();
        if count == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: expected {expected} hutan-client threads, found {count}: {:?}",
            thread_names()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn dead_end_options() -> ConnectOptions {
    ConnectOptions {
        endpoint: Some(PathBuf::from("/nonexistent/hutan-test.sock").into()),
        autostart: None,
        client: ClientInfo {
            kind: ClientKind::Desktop,
            version: "test".into(),
            origin: None,
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_joins_every_client_thread() {
    // The enumeration itself must see named threads, or the zero counts below prove nothing.
    let names = thread_names();
    assert!(
        names.iter().any(|n| n.starts_with("tokio-rt-worker")),
        "thread names are not visible: {names:?}"
    );
    assert_client_threads(0, "before the test");

    let env = Env::start().await;
    let connected = connect(&env);
    let lonely = Connection::start(dead_end_options());
    let dropped = Connection::start(dead_end_options());
    let mut state = connected.state();
    wait_connected(&mut state).await;
    assert_client_threads(3, "three connections");

    // A terminal handle that outlives its connection must not keep anything running.
    let handle = connected.attach(hutan_core::TerminalId(1), None);

    // The last clone going away stops the thread without an explicit shutdown.
    drop(dropped);
    assert_client_threads(2, "dropping the last clone");

    // Shutdown applies to every clone, and is complete when it returns.
    let clone = lonely.clone();
    lonely.shutdown();
    assert_client_threads(1, "shutdown of a cloned connection");
    assert_eq!(clone.state().borrow().connection, ConnectionState::Stopped);
    drop(clone);

    let mut observer = connected.state();
    connected.shutdown();
    assert_client_threads(0, "after the last shutdown");
    assert_eq!(
        observer.borrow_and_update().connection,
        ConnectionState::Stopped
    );
    drop(handle);

    // A pile of short-lived connections leaves nothing behind either.
    for _ in 0..5 {
        let connection = Connection::start(dead_end_options());
        tokio::time::sleep(Duration::from_millis(20)).await;
        connection.shutdown();
    }
    assert_client_threads(0, "after short-lived connections");
}
