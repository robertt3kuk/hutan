//! End-to-end flood bench (roadmap M1.7): an in-process daemon on a temporary `HUTAN_HOME`
//! runs one session that prints ~200 MB of base64 text, with two clients attached to its
//! terminal. One client acks every render, the other never acks. While the flood runs a third
//! connection samples `daemon.status` latency every 50 ms.
//!
//! `cargo run --release -p hutan-daemon --example flood`
//!
//! Prints one summary line and exits non-zero unless: the acking client ends with the screen
//! the daemon holds, the non-acking client received nothing after its snapshot, the status p99
//! is below 50 ms, and peak RSS (this whole process: daemon and clients) is below 120 MiB.
#![allow(unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use hutan_client::{Client, ConnectOptions};
use hutan_core::TerminalId;
use hutan_daemon::{DaemonConfig, Paths};
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonStatusInfo, Empty, Request, SessionCreate, SessionRef,
    SessionResult, SessionSpec, TerminalAck, TerminalAttach, TerminalSize, TerminalSpec,
};
use hutan_protocol::render::{Row, TerminalRender};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};

const SCRIPT: &str = "head -c 150000000 /dev/urandom | base64; exec cat";
/// What `cat` echoes back once `base64` is done. Not in the base64 alphabet, so it cannot occur
/// in the flood itself.
const MARKER: &str = "FLOOD-DONE-7f3a";
/// The marker line was typed before the flood started, so the PTY echoed it at once. Seeing it
/// again only counts as the end once this much scrollback exists (the echo is long gone).
const MIN_HISTORY: u32 = 1000;
/// `head -c 150000000 | base64`: 76 characters + newline per 57 input bytes.
const OUTPUT_BYTES: u64 = 150_000_000 / 57 * 77;
const SAMPLE_EVERY: Duration = Duration::from_millis(50);
const QUIET: Duration = Duration::from_millis(500);
const DEADLINE: Duration = Duration::from_secs(240);

const MAX_P99: Duration = Duration::from_millis(50);
const MAX_RSS_BYTES: u64 = 120 * 1024 * 1024;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let home = std::env::temp_dir().join(format!("hutan-flood-{}", std::process::id()));
    std::fs::create_dir_all(&home).context("creating the temporary HUTAN_HOME")?;
    let result = run(&home).await;
    let _ = std::fs::remove_dir_all(&home);
    result
}

async fn run(home: &std::path::Path) -> Result<()> {
    let paths = Paths::in_home(home);
    let daemon = hutan_daemon::start(DaemonConfig::new(paths.clone())).await?;
    let connect = || async {
        Client::connect(ConnectOptions {
            endpoint: Some(paths.socket.clone().into()),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Cli,
                version: "flood".to_owned(),
                origin: None,
            },
        })
        .await
        .context("connecting to the daemon")
    };
    let control = connect().await?;
    let acking = connect().await?;
    let silent = connect().await?;
    let sampler_client = connect().await?;

    let started = Instant::now();
    let created: SessionResult = control
        .call(Request::SessionCreate(SessionCreate {
            workspace_id: None,
            spec: SessionSpec::Terminal(TerminalSpec {
                command: ["sh", "-c", SCRIPT].map(str::to_owned).to_vec(),
                ..TerminalSpec::default()
            }),
        }))
        .await?;
    let session = created.session;
    let terminal = session.terminal_id.context("session has no terminal")?;
    // Queued on the PTY now, read by `cat` after the flood: comes back as the end marker.
    control
        .send_input(terminal, format!("{MARKER}\n").as_bytes())
        .await?;

    let (mut acking_rx, mut silent_rx) = (
        attach(&acking, terminal).await?,
        attach(&silent, terminal).await?,
    );
    let mut silent_renders = 0usize;
    let mut silent_screen = Screen::default();
    silent_renders += take_renders(&mut silent_rx, &mut silent_screen);

    let sampling = Arc::new(AtomicBool::new(true));
    let sampler = tokio::spawn({
        let sampling = Arc::clone(&sampling);
        async move {
            let mut latencies = Vec::new();
            while sampling.load(Ordering::Relaxed) {
                let sent = Instant::now();
                if sampler_client
                    .call::<DaemonStatusInfo>(Request::DaemonStatus(Empty {}))
                    .await
                    .is_err()
                {
                    break;
                }
                latencies.push(sent.elapsed());
                sleep(SAMPLE_EVERY).await;
            }
            latencies
        }
    });

    // The acking client: apply, then ack, until the marker shows up after the flood.
    let mut screen = Screen::default();
    let mut renders = 0usize;
    let finished = timeout(DEADLINE, async {
        loop {
            let render = acking_rx
                .recv()
                .await
                .context("acking client lost its stream")?;
            screen.apply(&render);
            renders += 1;
            acking
                .call::<Empty>(Request::TerminalAck(TerminalAck {
                    terminal_id: terminal,
                    frame: screen.frame,
                }))
                .await?;
            if screen.history_len >= MIN_HISTORY && screen.contains(MARKER) {
                return anyhow::Ok(());
            }
        }
    })
    .await;
    let wall = started.elapsed();
    sampling.store(false, Ordering::Relaxed);
    let latencies = sampler.await?;
    finished.context("the flood did not finish before the deadline")??;

    // Let the last frames land (still acking), then compare with what the daemon holds now.
    while let Ok(Some(render)) = timeout(QUIET, acking_rx.recv()).await {
        screen.apply(&render);
        renders += 1;
        acking
            .call::<Empty>(Request::TerminalAck(TerminalAck {
                terminal_id: terminal,
                frame: screen.frame,
            }))
            .await?;
    }
    silent_renders += take_renders(&mut silent_rx, &mut silent_screen);
    let fresh = connect().await?;
    let mut fresh_rx = attach(&fresh, terminal).await?;
    let mut truth = Screen::default();
    truth.apply(&fresh_rx.recv().await.context("no snapshot for the check")?);
    let final_screen_ok = truth.lines == screen.lines && truth.contains(MARKER);

    let peak_rss = peak_rss_bytes();
    let (p50, p99) = percentiles(latencies.clone());

    control
        .call::<SessionResult>(Request::SessionStop(SessionRef {
            session_id: session.id,
        }))
        .await?;
    daemon.shutdown();
    daemon.wait().await?;

    // The snapshot spent the silent client's only credit.
    let silent_after_snapshot = silent_renders.saturating_sub(1);
    let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    println!(
        "flood: wall={:.1}s output~{:.0}MiB ({:.0}MiB/s) acking_renders={} silent_renders_after_snapshot={} \
         status_p50={:.2}ms p99={:.2}ms samples={} peak_rss={:.1}MiB final_screen={}",
        wall.as_secs_f64(),
        mb(OUTPUT_BYTES),
        mb(OUTPUT_BYTES) / wall.as_secs_f64(),
        renders,
        silent_after_snapshot,
        ms(p50),
        ms(p99),
        latencies.len(),
        mb(peak_rss),
        if final_screen_ok { "match" } else { "MISMATCH" },
    );

    let mut failures = Vec::new();
    if !final_screen_ok {
        failures.push("acking client's final screen differs from the daemon's".to_owned());
    }
    if silent_renders != 1 {
        failures.push(format!(
            "non-acking client received {silent_renders} renders (expected only its snapshot)"
        ));
    }
    if p99 >= MAX_P99 {
        failures.push(format!("daemon.status p99 {:.2} ms >= 50 ms", ms(p99)));
    }
    if peak_rss >= MAX_RSS_BYTES {
        failures.push(format!("peak RSS {:.1} MiB >= 120 MiB", mb(peak_rss)));
    }
    if !failures.is_empty() {
        bail!("pass criteria not met:\n  {}", failures.join("\n  "));
    }
    Ok(())
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn percentiles(mut samples: Vec<Duration>) -> (Duration, Duration) {
    if samples.is_empty() {
        return (Duration::ZERO, Duration::ZERO);
    }
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).ceil() as usize];
    (at(0.5), at(0.99))
}

/// Attaches at 80x24 and returns the render stream (the snapshot is queued before the reply).
async fn attach(client: &Client, terminal: TerminalId) -> Result<mpsc::Receiver<TerminalRender>> {
    let frames = client.terminal_frames(terminal);
    client
        .call::<hutan_protocol::messages::TerminalAttached>(Request::TerminalAttach(
            TerminalAttach {
                terminal_id: terminal,
                size: Some(TerminalSize {
                    cols: 80,
                    rows: 24,
                    pixel_width: 0,
                    pixel_height: 0,
                }),
            },
        ))
        .await?;
    Ok(frames)
}

/// Applies whatever is queued on `rx` without waiting; returns how many renders that was.
fn take_renders(rx: &mut mpsc::Receiver<TerminalRender>, screen: &mut Screen) -> usize {
    let mut count = 0;
    while let Ok(render) = rx.try_recv() {
        screen.apply(&render);
        count += 1;
    }
    count
}

/// A client's picture of the terminal, rebuilt from snapshots and frames only.
#[derive(Debug, Default)]
struct Screen {
    lines: Vec<String>,
    frame: u64,
    history_len: u32,
}

impl Screen {
    fn apply(&mut self, render: &TerminalRender) {
        match render {
            TerminalRender::Snapshot(snapshot) => {
                self.lines = snapshot.lines.iter().map(row_text).collect();
                self.frame = snapshot.frame;
                self.history_len = snapshot.history_len;
            }
            TerminalRender::Frame(frame) => {
                for (index, row) in &frame.dirty {
                    if let Some(line) = self.lines.get_mut(usize::from(*index)) {
                        *line = row_text(row);
                    }
                }
                self.frame = frame.frame;
                self.history_len = frame.history_len;
            }
        }
    }

    fn contains(&self, needle: &str) -> bool {
        self.lines.iter().any(|line| line.contains(needle))
    }
}

fn row_text(row: &Row) -> String {
    row.runs.iter().map(|run| run.text.as_str()).collect()
}

/// Peak resident set size of this process in bytes (`ru_maxrss` is bytes on macOS).
fn peak_rss_bytes() -> u64 {
    // SAFETY: `getrusage` fills the zero-initialised struct we pass; no other preconditions.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return 0;
        }
        let max = u64::try_from(usage.ru_maxrss).unwrap_or(0);
        if cfg!(target_os = "linux") {
            max * 1024
        } else {
            max
        }
    }
}
