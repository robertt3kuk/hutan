//! Descriptor hygiene: a spawn/terminate/wait cycle must not leak file descriptors. Lives in
//! its own test binary so no other test opens descriptors concurrently in the same process.
#![allow(clippy::expect_used)] // tests assert by panicking

use std::time::Duration;

use hutan_protocol::messages::TerminalSize;
use hutan_terminal::{Terminal, TerminalConfig};
use tokio::time::timeout;

fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").expect("read /dev/fd").count()
}

fn config() -> TerminalConfig {
    TerminalConfig {
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()],
        size: TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
        },
        ..TerminalConfig::default()
    }
}

// One test function: counts are process-wide, so the scenarios must not run concurrently.
#[tokio::test(flavor = "current_thread")]
async fn descriptors_return_to_the_initial_count() {
    // Warm up one-time allocations of descriptors (runtime, thread machinery).
    let warmup = Terminal::spawn(config()).expect("spawn");
    warmup.terminate(Duration::from_secs(1)).expect("terminate");
    timeout(Duration::from_secs(15), warmup.wait_exit())
        .await
        .expect("exit");
    drop(warmup);

    let before = open_fds();
    for round in 0..5 {
        let term = Terminal::spawn(config()).expect("spawn");
        assert!(
            open_fds() > before,
            "round {round}: a live terminal holds descriptors"
        );
        term.terminate(Duration::from_secs(1)).expect("terminate");
        timeout(Duration::from_secs(15), term.wait_exit())
            .await
            .expect("exit");
        assert_eq!(
            open_fds(),
            before,
            "round {round}: descriptors leaked after wait_exit"
        );
    }

    // Dropping the last handle hangs the child up and releases everything too.
    let term = Terminal::spawn(config()).expect("spawn");
    let clone = term.clone();
    let pid = term.pid().expect("pid");
    drop(term);
    assert!(clone.pid().is_some(), "a clone keeps the terminal alive");
    drop(clone);
    // Teardown is asynchronous (the waiter thread reaps and releases): poll briefly.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while open_fds() != before && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        open_fds(),
        before,
        "descriptors released after drop (pid {pid})"
    );
}
