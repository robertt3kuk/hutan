//! Flood microbench for the three paths P-3 sped up (docs/perf.md, "P-3"): a `yes` flood
//! (one-cell rows spilled to the cold store), colored numbered lines, and a prompt-marker
//! corpus (OSC 7 / OSC 133 around every command, as the shell integration emits). Prints the
//! best of three runs in MB per CPU second of this process (a busy machine does not skew it as
//! it does wall time) for each; engine path only, no PTY, read size of the daemon.
//!
//! `cargo run --release -p hutan-terminal --example corpora [-- <megabytes>]`
#![allow(unsafe_code)] // one `clock_gettime` call

use std::time::Duration;

use hutan_protocol::messages::TerminalSize;
use hutan_terminal::Emulator;

/// CPU time this process has used so far.
fn cpu_time() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the duration of the call.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    if status != 0 {
        return Duration::ZERO;
    }
    Duration::new(
        u64::try_from(ts.tv_sec).unwrap_or(0),
        u32::try_from(ts.tv_nsec).unwrap_or(0),
    )
}

/// The daemon's PTY read buffer.
const READ: usize = 64 * 1024;

fn corpus(unit: &[u8], megabytes: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(megabytes * 1_000_000 + unit.len());
    while data.len() < megabytes * 1_000_000 {
        data.extend_from_slice(unit);
    }
    data
}

fn run(name: &str, data: &[u8]) {
    let size = TerminalSize {
        cols: 120,
        rows: 40,
        pixel_width: 0,
        pixel_height: 0,
    };
    let mut best = f64::MAX;
    let mut history = 0;
    let mut blocks = 0;
    for _ in 0..3 {
        let Ok(mut emulator) = Emulator::new(size, 10_000) else {
            return;
        };
        let start = cpu_time();
        for read in data.chunks(READ) {
            emulator.feed(read);
            let _ = emulator.drain_replies();
            let _ = emulator.drain_meta();
        }
        best = best.min((cpu_time() - start).as_secs_f64());
        history = emulator.history_len();
        blocks = emulator.blocks(None, usize::MAX).len();
    }
    println!(
        "{name:<16} {:>7.1} MB/CPU-s  ({:.2} CPU-s for {:.0} MB; history {history}, blocks {blocks})",
        data.len() as f64 / 1e6 / best,
        best,
        data.len() as f64 / 1e6,
    );
}

fn main() {
    let megabytes = std::env::args()
        .nth(1)
        .and_then(|arg| arg.parse().ok())
        .unwrap_or(200);
    run("yes", &corpus(b"y\r\n", megabytes));

    let mut lines = Vec::new();
    for n in 0..5000 {
        lines.extend_from_slice(
            format!(
                "\x1b[3{}mline {n} some colored text that is a bit longer\x1b[0m\r\n",
                n % 8
            )
            .as_bytes(),
        );
    }
    run("colored lines", &corpus(&lines, megabytes));

    let mut markers = Vec::new();
    for n in 0..2000 {
        markers.extend_from_slice(
            format!(
                "\x1b]7;file://host/tmp/project-{n}\x07\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07a b c\r\nd e f\r\n\x1b]133;D;0\x07"
            )
            .as_bytes(),
        );
    }
    run("prompt markers", &corpus(&markers, megabytes / 4));
}
