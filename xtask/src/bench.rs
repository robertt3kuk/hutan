//! `xtask bench`: measures the performance budgets of `docs/vision.md` (B1–B6) end to end.
//!
//! Every scenario runs against its own daemon in a temporary `HUTAN_HOME`, with the real
//! release `hutan-desktop` (a window opens; nothing is captured). The desktop is driven by a
//! seeded `desktop-state.json` and, for the latency scenarios, by `HUTAN_BENCH_INPUT`; its
//! `HUTAN_TRACE_FILE` is the measurement channel (see `hutan_terminal_view::trace`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use hutan_client::{Client, ConnectOptions};
use hutan_core::TerminalId;
use hutan_protocol::messages::{
    ClientInfo, ClientKind, DaemonStatusInfo, Empty, Request, SessionCreate, SessionInfo,
    SessionResult, SessionSpec, TerminalSize, TerminalSpec,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::sleep;

use crate::proc::{self, Guard};
use crate::seed;
use crate::stats::{median, percentile};

pub(crate) const MIB: f64 = 1024.0 * 1024.0;
/// Metric holding the refresh interval of the window's display (ms), read from the platform.
pub(crate) const REFRESH_INTERVAL_MS: &str = "b4_refresh_interval_ms";
/// 1.0 when a typing run's window was not presented at the display rate (see `typing`).
pub(crate) const DISPLAY_STALLED: &str = "b4_display_stalled";
/// Attempts per run of a scenario whose measurement the machine's display state can spoil.
const MAX_ATTEMPTS: u32 = 4;

/// A budget from `docs/vision.md` and the metric that decides it.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub id: &'static str,
    pub name: &'static str,
    pub metric: &'static str,
    pub unit: &'static str,
    /// Passes when the median is at most this.
    pub limit: f64,
    /// When set, `limit` is not fixed: it is the display refresh interval measured with the
    /// run (median over runs of `b4_refresh_interval_ms`) plus this many milliseconds.
    pub frame_margin_ms: Option<f64>,
}

pub const BUDGETS: &[Budget] = &[
    Budget {
        id: "B1",
        name: "Idle desktop RSS, 1 window, 4 terminals",
        metric: "b1_desktop_rss_mb",
        unit: "MB",
        limit: 150.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B2",
        name: "Idle daemon RSS, 10 sessions x 10k lines, 80 cols",
        metric: "b2_daemon_rss_mb",
        unit: "MB",
        limit: 120.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B2w",
        name: "Idle daemon RSS, 10 sessions x 10k lines, 200 cols",
        metric: "b2w_daemon_rss_mb",
        unit: "MB",
        limit: 150.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B3",
        name: "Idle CPU, daemon + desktop, 30 s",
        metric: "b3_idle_cpu_pct",
        unit: "%",
        limit: 0.5,
        frame_margin_ms: None,
    },
    Budget {
        id: "B4a",
        name: "Hutan path: key handled to echo frame submitted, p95",
        metric: "b4a_key_p95_ms",
        unit: "ms",
        limit: 2.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B4b",
        name: "Keystroke to glyph on screen, p95 (one frame + 2 ms)",
        metric: "b4b_key_p95_ms",
        unit: "ms",
        // ADR 0011: one display frame plus 2 ms; the frame is the refresh interval the platform
        // reported for the window's display, recorded per run as `b4_refresh_interval_ms`.
        // Not a fixed number: see `frame_margin_ms`.
        limit: 0.0,
        frame_margin_ms: Some(2.0),
    },
    Budget {
        id: "B5",
        name: "Flood: p95 key latency in a second terminal",
        metric: "b5_key_p95_ms",
        unit: "ms",
        limit: 50.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B5",
        name: "Flood: daemon RSS growth",
        metric: "b5_daemon_growth_mb",
        unit: "MB",
        limit: 50.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B6",
        name: "Cold start to first terminal text painted",
        metric: "b6_cold_start_ms",
        unit: "ms",
        limit: 500.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B3p",
        name: "Idle CPU, 60 sessions + Tasks, Resources, Agents open, 30 s",
        metric: "b3p_idle_cpu_pct",
        unit: "%",
        limit: 0.5,
        frame_margin_ms: None,
    },
    Budget {
        id: "B7a",
        name: "Chat of 5000 events: open to first paint",
        metric: "b7a_open_ms",
        unit: "ms",
        limit: 150.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B7b",
        name: "Chat of 5000 events: scroll frame p95 (one frame + 2 ms)",
        metric: "b7b_scroll_frame_p95_ms",
        unit: "ms",
        limit: 0.0,
        frame_margin_ms: Some(2.0),
    },
    Budget {
        id: "B7c",
        name: "Chat of 5000 events: desktop RSS growth",
        metric: "b7c_rss_growth_mb",
        unit: "MB",
        limit: 60.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B8a",
        name: "Streaming 200 deltas/s: frame p95 (one frame + 2 ms)",
        metric: "b8a_frame_p95_ms",
        unit: "ms",
        limit: 0.0,
        frame_margin_ms: Some(2.0),
    },
    Budget {
        id: "B8b",
        name: "Streaming 200 deltas/s: longest frame",
        metric: "b8b_frame_max_ms",
        unit: "ms",
        limit: 50.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B8c",
        name: "Streaming 200 deltas/s: composer key p95 (Hutan path)",
        metric: "b8c_key_p95_ms",
        unit: "ms",
        limit: 16.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B9a",
        name: "Sidebar, 300 sessions / 40 projects: filter key p95 (Hutan path)",
        metric: "b9a_key_p95_ms",
        unit: "ms",
        limit: 16.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B9b",
        name: "Sidebar, 300 sessions: drag frame p95 (one frame + 2 ms)",
        metric: "b9b_frame_p95_ms",
        unit: "ms",
        limit: 0.0,
        frame_margin_ms: Some(2.0),
    },
    Budget {
        id: "B10",
        name: "Switch between 10 terminals and 5 chats: p95 to first frame",
        metric: "b10_switch_p95_ms",
        unit: "ms",
        limit: 50.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B11a",
        name: "Open a 1 MB Rust file in a pane: dispatch to first paint of its text",
        metric: "b11a_open_1mb_ms",
        unit: "ms",
        limit: 100.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B11b",
        name: "Open a 10 MB Rust file in a pane: dispatch to first paint of its text",
        metric: "b11b_open_10mb_ms",
        unit: "ms",
        limit: 500.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B11c",
        name: "1 MB Rust file: highlighted viewport after the first paint",
        metric: "b11c_highlighted_after_ms",
        unit: "ms",
        limit: 400.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B11d",
        name: "Desktop RSS growth, one open 1 MB file with syntax",
        metric: "b11d_rss_growth_mb",
        unit: "MB",
        limit: 40.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B12a",
        name: "Typing into a 1 MB Rust file with syntax: key handled to frame submitted (Hutan path), p95",
        metric: "b12a_key_p95_ms",
        unit: "ms",
        limit: 2.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B12w",
        name: "Typing into a 10 000-line Markdown file with soft wrap on: key handled to frame submitted (Hutan path), p95",
        metric: "b12w_key_p95_ms",
        unit: "ms",
        limit: 2.0,
        frame_margin_ms: None,
    },
    Budget {
        id: "B13",
        name: "Continuous scroll of a 1 MB file: frame interval p95 (one frame + 2 ms)",
        metric: "b13_scroll_frame_p95_ms",
        unit: "ms",
        limit: 0.0,
        frame_margin_ms: Some(2.0),
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// B1, B2, B3 in one run: 10 loaded sessions, 4 of them on screen, then idle.
    Idle,
    /// B2w: 10 loaded sessions of 200 columns, no client.
    IdleWide,
    /// B4a, B4b.
    Typing,
    /// B5.
    Flood,
    /// B6.
    ColdStart,
    /// B3p: idle with the sidebar full and the Tasks, Resources and Agents panels open.
    IdlePanels,
    /// B7a, B7b, B7c: a chat of 5000 journal events opened and scrolled.
    Transcript,
    /// B8a, B8b, B8c: a reply streams while the composer is typed into.
    Streaming,
    /// B9a, B9b: the sidebar with 300 sessions: filter typing and a drag.
    Sidebar,
    /// B10: switching between ten terminals and five chats.
    Switching,
    /// B11a-d: a 1 MB and a 10 MB Rust file opened in a tab.
    EditorOpen,
    /// B13: a 1 MB Rust file scrolled.
    EditorScroll,
    /// B12a: typing into a 1 MB Rust file.
    EditorType,
    /// B12w: typing into a 10 000-line Markdown file that wraps.
    EditorTypeWrap,
}

impl Scenario {
    const ALL: [Scenario; 14] = [
        Scenario::Idle,
        Scenario::IdleWide,
        Scenario::Typing,
        Scenario::Flood,
        Scenario::ColdStart,
        Scenario::IdlePanels,
        Scenario::Transcript,
        Scenario::Streaming,
        Scenario::Sidebar,
        Scenario::Switching,
        Scenario::EditorOpen,
        Scenario::EditorScroll,
        Scenario::EditorType,
        Scenario::EditorTypeWrap,
    ];

    fn name(self) -> &'static str {
        match self {
            Scenario::Idle => "idle (B1,B2,B3)",
            Scenario::IdleWide => "idle wide (B2w)",
            Scenario::Typing => "typing (B4a,B4b)",
            Scenario::Flood => "flood (B5)",
            Scenario::ColdStart => "cold start (B6)",
            Scenario::IdlePanels => "idle with panels (B3p)",
            Scenario::Transcript => "transcript (B7)",
            Scenario::Streaming => "streaming (B8)",
            Scenario::Sidebar => "sidebar (B9)",
            Scenario::Switching => "switching (B10)",
            Scenario::EditorOpen => "editor open (B11)",
            Scenario::EditorScroll => "editor scroll (B13)",
            Scenario::EditorType => "editor typing (B12a)",
            Scenario::EditorTypeWrap => "editor typing, soft wrap (B12w)",
        }
    }

    fn budgets(self) -> &'static [&'static str] {
        match self {
            Scenario::Idle => &["B1", "B2", "B3"],
            Scenario::IdleWide => &["B2w"],
            Scenario::Typing => &["B4a", "B4b"],
            Scenario::Flood => &["B5"],
            Scenario::ColdStart => &["B6"],
            Scenario::IdlePanels => &["B3p"],
            Scenario::Transcript => &["B7a", "B7b", "B7c"],
            Scenario::Streaming => &["B8a", "B8b", "B8c"],
            Scenario::Sidebar => &["B9a", "B9b"],
            Scenario::Switching => &["B10"],
            Scenario::EditorOpen => &["B11a", "B11b", "B11c", "B11d"],
            Scenario::EditorScroll => &["B13"],
            Scenario::EditorType => &["B12a"],
            Scenario::EditorTypeWrap => &["B12w"],
        }
    }
}

/// Whether the `--only` entry `wanted` (any case) selects the budget `id`: exactly, or as the
/// family prefix (`B4` selects `B4a` and `B4b`, `B2` selects `B2w`, `B3` selects `B3p`; `B1`
/// does not select `B10`: a family's members end in a letter).
fn selects(wanted: &str, id: &str) -> bool {
    let family_member = id.chars().last().is_some_and(|c| c.is_ascii_lowercase());
    let (wanted, id) = (wanted.to_uppercase(), id.to_uppercase());
    id == wanted
        || (family_member
            && wanted.len() >= 2
            && id.len() == wanted.len() + 1
            && id.starts_with(&wanted))
}

#[derive(Debug)]
pub struct Options {
    /// Budget ids to measure (`B1`…); empty means all.
    pub only: Vec<String>,
    pub json: bool,
    pub runs: usize,
    /// Profile the desktop and daemon with `sample(1)` during the latency scenarios.
    pub sample: bool,
    /// Milliseconds between keystrokes of the typing and flood scenarios (B4, B5); defaults
    /// [`TYPING_INTERVAL_MS`] and [`FLOOD_INTERVAL_MS`].
    pub key_interval_ms: Option<u64>,
}

pub(crate) type Metrics = BTreeMap<String, f64>;

pub async fn run(options: Options) -> Result<()> {
    let root = workspace_root();
    let wanted: Vec<String> = options.only.clone();
    for id in &wanted {
        if !BUDGETS.iter().any(|b| selects(id, b.id)) {
            bail!(
                "unknown budget {id}; known: B1, B2, B2w, B3, B3p, B4a, B4b, B5, B6, B7a, B7b, B7c, B8a, B8b, B8c, B9a, B9b, B10, B11a, B11b, B11c, B11d, B12a, B12w, B13"
            );
        }
    }
    let selected = |s: &Scenario| {
        wanted.is_empty()
            || s.budgets()
                .iter()
                .any(|b| wanted.iter().any(|w| selects(w, b)))
    };
    let scenarios: Vec<Scenario> = Scenario::ALL.into_iter().filter(selected).collect();

    let mut bins = build(&root)?;
    if options.sample {
        bins.sample_dir = Some(root.join("target/bench"));
    }
    keep_display_awake();
    let machine = machine_info();
    let mut results: BTreeMap<&'static str, Vec<Metrics>> = BTreeMap::new();
    // A scenario that fails keeps what the others measured: the report is written either way.
    let outcome = run_scenarios(&scenarios, &bins, &options, &mut results).await;

    let rows = summarize(&results, &wanted);
    let report = json!({
        "timestamp": unix_secs(),
        "machine": machine,
        "runs": options.runs,
        "budgets": rows.iter().map(Row::to_json).collect::<Vec<_>>(),
        "scenarios": results.iter().map(|(name, runs)| (name.to_string(), json!(runs))).collect::<serde_json::Map<_, _>>(),
    });
    let dir = root.join("target/bench");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", unix_secs()));
    std::fs::write(&path, serde_json::to_string_pretty(&report)?)?;
    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_table(&rows);
    }
    eprintln!("wrote {}", path.display());
    outcome?;
    if rows.iter().any(|r| !r.pass) {
        bail!("at least one budget missed");
    }
    Ok(())
}

async fn run_scenarios(
    scenarios: &[Scenario],
    bins: &Bins,
    options: &Options,
    results: &mut BTreeMap<&'static str, Vec<Metrics>>,
) -> Result<()> {
    for &scenario in scenarios {
        for run in 1..=options.runs {
            let mut attempt = 1;
            let measured = loop {
                eprintln!(
                    "== {} run {run}/{} (attempt {attempt})",
                    scenario.name(),
                    options.runs
                );
                let mut measured = tokio::select! {
                    result = run_scenario(scenario, bins, options.key_interval_ms) => result?,
                    _ = tokio::signal::ctrl_c() => bail!("interrupted"),
                };
                for budget in BUDGETS
                    .iter()
                    .filter(|b| scenario.budgets().contains(&b.id))
                {
                    if let Some(value) = measured.get(budget.metric) {
                        eprintln!("   {:<4} {value:.2} {}", budget.id, budget.unit);
                    }
                }
                let stalled = measured.get(DISPLAY_STALLED) == Some(&1.0);
                if stalled {
                    eprintln!(
                        "the window was inactive or not presented at the display rate during this run (another app in front, occluded, display asleep?)"
                    );
                    if attempt >= MAX_ATTEMPTS {
                        bail!(
                            "{} run {run}: the window was not presented at the display rate in {MAX_ATTEMPTS} attempts; keep the bench window in front and the display awake",
                            scenario.name()
                        );
                    }
                    attempt += 1;
                    continue;
                }
                measured.insert("attempts".into(), f64::from(attempt));
                break measured;
            };
            results.entry(scenario.name()).or_default().push(measured);
        }
    }
    Ok(())
}

/// Prevents display and idle sleep while the bench runs (`caffeinate -d -i -w <our pid>`: it
/// exits when xtask does). A sleeping display has no display link, which spoils every latency
/// scenario (GPUI fails with `could not create display link`). Best effort: without
/// `caffeinate` (not macOS) the bench runs as it did.
fn keep_display_awake() {
    let _ = Command::new("caffeinate")
        .args(["-d", "-i", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

pub(crate) struct Bins {
    desktop: PathBuf,
    daemon: PathBuf,
    cli: PathBuf,
    /// Where `--sample` writes `sample(1)` output; `None` without the flag.
    sample_dir: Option<PathBuf>,
}

fn build(root: &Path) -> Result<Bins> {
    eprintln!("building release binaries");
    let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "hutan-desktop",
            "-p",
            "hutan-daemon",
            "-p",
            "hutan-cli",
        ])
        .current_dir(root)
        .status()
        .context("running cargo build")?;
    if !status.success() {
        bail!("cargo build failed");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| root.join("target"), PathBuf::from)
        .join("release");
    let bins = Bins {
        desktop: target.join("hutan-desktop"),
        daemon: target.join("hutand"),
        cli: target.join("hutan"),
        sample_dir: None,
    };
    for bin in [&bins.desktop, &bins.daemon, &bins.cli] {
        if !bin.is_file() {
            bail!("missing {}", bin.display());
        }
    }
    Ok(bins)
}

fn machine_info() -> Value {
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    };
    json!({
        "cpu": run("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "memory_bytes": run("sysctl", &["-n", "hw.memsize"]).parse::<u64>().unwrap_or(0),
        "os": format!("macOS {}", run("sw_vers", &["-productVersion"])),
        "rustc": run("rustc", &["--version"]),
    })
}

// ---- report ------------------------------------------------------------------------------

struct Row {
    budget: Budget,
    /// The budget's limit, resolved for this report (B4b depends on the display).
    limit: f64,
    values: Vec<f64>,
    median: f64,
    pass: bool,
}

impl Row {
    fn to_json(&self) -> Value {
        json!({
            "id": self.budget.id,
            "name": self.budget.name,
            "metric": self.budget.metric,
            "unit": self.budget.unit,
            "limit": self.limit,
            "values": self.values,
            "median": self.median,
            "pass": self.pass,
        })
    }
}

fn summarize(results: &BTreeMap<&'static str, Vec<Metrics>>, wanted: &[String]) -> Vec<Row> {
    let all = || results.values().flatten();
    BUDGETS
        .iter()
        .filter(|b| wanted.is_empty() || wanted.iter().any(|w| selects(w, b.id)))
        .filter_map(|budget| {
            let values: Vec<f64> = all()
                .filter_map(|m| m.get(budget.metric).copied())
                .collect();
            let med = median(&values)?;
            let limit = match budget.frame_margin_ms {
                Some(margin) => {
                    let frames: Vec<f64> = all()
                        .filter_map(|m| m.get(REFRESH_INTERVAL_MS).copied())
                        .collect();
                    median(&frames)? + margin
                }
                None => budget.limit,
            };
            Some(Row {
                budget: *budget,
                limit,
                pass: med <= limit,
                median: med,
                values,
            })
        })
        .collect()
}

fn print_table(rows: &[Row]) {
    println!(
        "{:<3} {:<54} {:>9} {:>10}  {:<28} result",
        "id", "metric", "budget", "median", "runs"
    );
    for row in rows {
        let runs: Vec<String> = row.values.iter().map(|v| format!("{v:.2}")).collect();
        println!(
            "{:<3} {:<54} {:>9} {:>10}  {:<28} {}",
            row.budget.id,
            row.budget.name,
            format!("<= {:.1} {}", row.limit, row.budget.unit),
            format!("{:.2} {}", row.median, row.budget.unit),
            runs.join(", "),
            if row.pass { "PASS" } else { "FAIL" },
        );
    }
}

// ---- stack: one daemon (+ desktop) in a temporary home ----------------------------------

pub(crate) struct Stack<'a> {
    bins: &'a Bins,
    home: TempDir,
    pub(crate) client: Client,
    // Declared after `client` so the connection closes before the daemon is stopped.
    pub(crate) daemon: Guard,
    desktop: Option<Guard>,
}

impl<'a> Stack<'a> {
    async fn start(bins: &'a Bins) -> Result<Self> {
        Self::start_with(bins, None).await
    }

    /// Like [`start`](Self::start), with `daemon_toml` as the daemon's configuration file.
    pub(crate) async fn start_with(bins: &'a Bins, daemon_toml: Option<&str>) -> Result<Self> {
        let home = tempfile::Builder::new()
            .prefix("hutan-bench-")
            .tempdir()
            .context("creating the temporary HUTAN_HOME")?;
        if let Some(text) = daemon_toml {
            std::fs::write(home.path().join("daemon.toml"), text)?;
        }
        let log = std::fs::File::create(home.path().join("daemon.out"))?;
        let mut daemon = Guard::spawn(
            "hutand",
            Command::new(&bins.daemon)
                .env_remove("HUTAN_SOCKET")
                .env("HUTAN_HOME", home.path())
                .env("HUTAN_LOG", "warn")
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log),
        )?;
        let socket = home.path().join("hutand.sock");
        let started = Instant::now();
        let client = loop {
            daemon.ensure_running()?;
            let attempt = Client::connect(ConnectOptions {
                endpoint: Some(socket.clone().into()),
                autostart: None,
                client: ClientInfo {
                    kind: ClientKind::Cli,
                    version: "xtask-bench".to_owned(),
                    origin: None,
                },
            })
            .await;
            match attempt {
                Ok(client) => break client,
                Err(err) if started.elapsed() > Duration::from_secs(15) => {
                    bail!("cannot reach the daemon: {err}")
                }
                Err(_) => sleep(Duration::from_millis(50)).await,
            }
        };
        Ok(Self {
            bins,
            home,
            client,
            daemon,
            desktop: None,
        })
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.home.path().join(name)
    }

    /// With `--sample`: runs `sample <pid> <secs>` in the background, writing
    /// `target/bench/sample-<timestamp>-<name>.txt`. Wait for the returned child.
    pub(crate) fn sample(&self, pid: u32, secs: u32, name: &str) -> Option<std::process::Child> {
        let dir = self.bins.sample_dir.as_ref()?;
        let _ = std::fs::create_dir_all(dir);
        let file = dir.join(format!("sample-{}-{name}.txt", unix_secs()));
        eprintln!("sampling {name} ({pid}) for {secs}s -> {}", file.display());
        Command::new("sample")
            .arg(pid.to_string())
            .arg(secs.to_string())
            .arg("1")
            .arg("-file")
            .arg(file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    pub(crate) async fn create(&self, command: &[&str]) -> Result<SessionInfo> {
        self.create_sized(command, None).await
    }

    pub(crate) async fn create_sized(
        &self,
        command: &[&str],
        size: Option<TerminalSize>,
    ) -> Result<SessionInfo> {
        let result: SessionResult = self
            .client
            .call(Request::SessionCreate(SessionCreate {
                workspace_id: None,
                spec: SessionSpec::Terminal(TerminalSpec {
                    command: command.iter().map(|s| (*s).to_owned()).collect(),
                    size,
                    ..TerminalSpec::default()
                }),
            }))
            .await?;
        Ok(result.session)
    }

    pub(crate) async fn type_into(&self, session: &SessionInfo, text: &str) -> Result<()> {
        let terminal: TerminalId = session.terminal_id.context("session has no terminal")?;
        self.client.send_input(terminal, text.as_bytes()).await?;
        Ok(())
    }

    pub(crate) async fn clients(&self) -> Result<u32> {
        let status: DaemonStatusInfo = self.client.call(Request::DaemonStatus(Empty {})).await?;
        Ok(status.clients)
    }

    pub(crate) fn daemon_usage(&self) -> Result<proc::Usage> {
        proc::usage(self.daemon.pid())
    }

    /// Seeds the layout and starts the desktop. `trace` enables `HUTAN_TRACE_LATENCY`;
    /// `script` is a key script for `HUTAN_BENCH_INPUT`.
    fn launch_desktop(
        &mut self,
        sessions: &[&SessionInfo],
        trace: bool,
        script: Option<&Value>,
    ) -> Result<()> {
        let ids: Vec<String> = sessions.iter().map(|s| s.id.to_string()).collect();
        self.launch_layout(&seed::layout(&ids)?, trace, script, None)
    }

    /// Writes `layout` as the desktop's saved state and starts the desktop; `ui` is a scenario
    /// for `HUTAN_BENCH_UI` (`bench_ui` of the desktop crate, which needs `trace`).
    pub(crate) fn launch_layout(
        &mut self,
        layout: &Value,
        trace: bool,
        script: Option<&Value>,
        ui: Option<&Value>,
    ) -> Result<()> {
        std::fs::write(
            self.path("desktop-state.json"),
            serde_json::to_string_pretty(layout)?,
        )?;
        // A fresh home shows the first-run setup over the whole window: a user who has set up
        // Hutan (everyone who uses it) does not see it, and it blocks the mouse.
        // `HUTAN_BENCH_SETTINGS`: more settings for the run (an A/B of one setting).
        let extra = std::env::var("HUTAN_BENCH_SETTINGS").unwrap_or_default();
        std::fs::write(
            self.path("settings.toml"),
            format!("[app]\nsetup_done = true\n{extra}\n"),
        )?;
        let log = std::fs::File::create(self.path("desktop.out"))?;
        let mut command = Command::new(&self.bins.desktop);
        command
            .env_remove("HUTAN_SOCKET")
            .env("HUTAN_HOME", self.home.path())
            .env("RUST_LOG", "info")
            .env("RUST_BACKTRACE", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        if trace {
            command
                .env("HUTAN_TRACE_LATENCY", "1")
                .env("HUTAN_TRACE_FILE", self.path("trace.jsonl"));
        }
        if let Some(script) = script {
            let path = self.path("keys.json");
            std::fs::write(&path, serde_json::to_string(script)?)?;
            command.env("HUTAN_BENCH_INPUT", path);
        }
        if let Some(ui) = ui {
            let path = self.path("scenario.json");
            std::fs::write(&path, serde_json::to_string(ui)?)?;
            command.env("HUTAN_BENCH_UI", path);
        }
        self.desktop = Some(Guard::spawn("hutan-desktop", &mut command)?);
        Ok(())
    }

    pub(crate) fn desktop(&mut self) -> Result<&mut Guard> {
        self.desktop.as_mut().context("the desktop is not running")
    }

    /// Fails when the desktop exited or did not restore the seeded layout.
    pub(crate) fn check_desktop(&mut self) -> Result<()> {
        self.desktop()?.ensure_running()?;
        let log = std::fs::read_to_string(self.path("desktop.out")).unwrap_or_default();
        if log.contains("using the default layout") {
            bail!("the desktop ignored the seeded layout:\n{log}");
        }
        Ok(())
    }

    pub(crate) fn trace(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("trace.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Waits for a trace event; the desktop must stay up.
    pub(crate) async fn wait_trace(
        &mut self,
        what: &str,
        timeout: Duration,
        pred: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = self.trace().into_iter().find(&pred) {
                return Ok(event);
            }
            self.check_desktop()?;
            if Instant::now() > deadline {
                bail!("timed out after {timeout:?} waiting for {what}");
            }
            sleep(Duration::from_millis(25)).await;
        }
    }

    /// What the daemon and the desktop printed, for failure reports.
    pub(crate) fn logs(&self) -> String {
        let tail = |name: &str| {
            let text = std::fs::read_to_string(self.path(name)).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(40)..].join("\n")
        };
        format!(
            "--- daemon.out ---\n{}\n--- desktop.out ---\n{}\n--- trace.jsonl (tail) ---\n{}",
            tail("daemon.out"),
            tail("desktop.out"),
            tail("trace.jsonl")
        )
    }

    /// Quits the desktop with SIGTERM (it saves its layout and leaves the daemon running), then
    /// stops the daemon and its sessions through `hutan daemon stop --stop-sessions`, the way a
    /// user would. Fails when either had to be SIGKILLed, crashed, or left a process behind.
    fn teardown(mut self) -> Result<()> {
        let desktop = self
            .desktop
            .take()
            .map(|mut desktop| desktop.stop(Duration::from_secs(10)).check());
        self.daemon.note_descendants();
        let cli = Command::new(&self.bins.cli)
            .args(["daemon", "stop", "--stop-sessions"])
            .env_remove("HUTAN_SOCKET")
            .env("HUTAN_HOME", self.home.path())
            .stdin(Stdio::null())
            .output()
            .context("running `hutan daemon stop`")?;
        if !cli.status.success() {
            eprintln!(
                "hutan daemon stop failed ({}): {}",
                cli.status,
                String::from_utf8_lossy(&cli.stderr)
            );
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.daemon.running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let daemon = self.daemon.stop(Duration::from_secs(10)).check();
        desktop.transpose()?;
        daemon
    }
}

fn startup_us(events: &[Value], stage: &str) -> Option<u64> {
    events
        .iter()
        .find(|e| e["ev"] == "startup" && e["stage"] == stage)
        .and_then(|e| e["t_us"].as_u64())
}

pub(crate) fn is_event(event: &Value, name: &str) -> bool {
    event["ev"] == name
}

fn key_stage_ms(events: &[Value], field: &str, window: Option<(u64, u64)>) -> Vec<f64> {
    events
        .iter()
        .filter(|e| is_event(e, "key"))
        .filter(|e| {
            window.is_none_or(|(from, to)| e["t_us"].as_u64().is_some_and(|t| from <= t && t <= to))
        })
        .filter_map(|e| e[field].as_f64())
        .map(|us| us / 1000.0)
        .collect()
}

fn key_totals_ms(events: &[Value], window: Option<(u64, u64)>) -> Vec<f64> {
    key_stage_ms(events, "total_us", window)
}

/// Per keystroke, what Hutan itself spends: key handled to the end of the paint that shows the
/// echo, minus the wait for the display tick in between (B4a, ADR 0011).
fn key_hutan_ms(events: &[Value]) -> Vec<f64> {
    events
        .iter()
        .filter(|e| is_event(e, "key"))
        .filter_map(|e| {
            let us = |field: &str| e[field].as_f64();
            let paint = (us("echo_to_paint_us")? - us("echo_to_prepaint_us")?).max(0.0);
            Some((us("key_to_sent_us")? + us("sent_to_echo_us")? + paint) / 1000.0)
        })
        .collect()
}

/// Median and p95 of each stage of the keystroke path.
fn record_stages(
    metrics: &mut Metrics,
    prefix: &str,
    events: &[Value],
    window: Option<(u64, u64)>,
) {
    for (name, field) in [
        ("key_to_sent", "key_to_sent_us"),
        ("sent_to_echo", "sent_to_echo_us"),
        ("echo_to_paint", "echo_to_paint_us"),
        ("echo_to_prepaint", "echo_to_prepaint_us"),
    ] {
        let values = key_stage_ms(events, field, window);
        for (suffix, p) in [("p50", 50.0), ("p95", 95.0)] {
            if let Some(v) = percentile(&values, p) {
                metrics.insert(format!("{prefix}_{name}_{suffix}_ms"), v);
            }
        }
    }
}

fn record_latency(metrics: &mut Metrics, prefix: &str, totals: &[f64]) {
    for (suffix, p) in [("p50", 50.0), ("p95", 95.0), ("p99", 99.0), ("max", 100.0)] {
        if let Some(v) = percentile(totals, p) {
            metrics.insert(format!("{prefix}_key_{suffix}_ms"), v);
        }
    }
    metrics.insert(format!("{prefix}_keys_measured"), totals.len() as f64);
}

pub(crate) fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

// ---- scenarios -----------------------------------------------------------------------------

async fn run_scenario(
    scenario: Scenario,
    bins: &Bins,
    key_interval_ms: Option<u64>,
) -> Result<Metrics> {
    let mut stack = match scenario {
        Scenario::Transcript | Scenario::Streaming | Scenario::Switching | Scenario::IdlePanels => {
            crate::bench_ui::start_stack(bins).await?
        }
        _ => Stack::start(bins).await?,
    };
    let result = match scenario {
        Scenario::Idle => idle(&mut stack).await,
        Scenario::IdleWide => idle_wide(&mut stack).await,
        Scenario::Typing => typing(&mut stack, key_interval_ms).await,
        Scenario::Flood => flood(&mut stack, key_interval_ms).await,
        Scenario::ColdStart => cold_start(&mut stack).await,
        Scenario::IdlePanels => crate::bench_ui::idle_panels(&mut stack).await,
        Scenario::Transcript => crate::bench_ui::transcript(&mut stack).await,
        Scenario::Streaming => crate::bench_ui::streaming(&mut stack).await,
        Scenario::Sidebar => crate::bench_ui::sidebar(&mut stack).await,
        Scenario::Switching => crate::bench_ui::switching(&mut stack).await,
        Scenario::EditorOpen => crate::bench_ui::editor_open(&mut stack).await,
        Scenario::EditorScroll => crate::bench_ui::editor_scroll(&mut stack).await,
        Scenario::EditorType => crate::bench_ui::editor_type(&mut stack).await,
        Scenario::EditorTypeWrap => crate::bench_ui::editor_type_wrap(&mut stack).await,
    };
    // The trace of the run stays next to the report: what the harness reads is what you can read.
    let trace = stack.path("trace.jsonl");
    if trace.is_file() {
        let dir = workspace_root().join("target/bench");
        let slug: String = scenario
            .name()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::copy(
            &trace,
            dir.join(format!("trace-{}-{slug}.jsonl", unix_secs())),
        );
    }
    let result = result.map_err(|err| err.context(stack.logs()));
    let teardown = stack.teardown();
    let metrics = result?;
    teardown?;
    Ok(metrics)
}

/// `sh` session that prints a prompt and then echoes what is typed, like a shell at rest.
pub(crate) const PROMPT_CAT: &[&str] = &["sh", "-c", "printf 'bench> '; exec cat"];

/// Seconds of idling before B1 is sampled, and the B3 window after it.
pub(crate) const SETTLE: Duration = Duration::from_secs(10);
pub(crate) const CPU_WINDOW: Duration = Duration::from_secs(30);
const LOADED_SESSIONS: usize = 10;
const SHOWN_SESSIONS: usize = 4;

/// Wide terminals for B2w: the history rows are as wide as the pane of a large window.
const WIDE_SIZE: TerminalSize = TerminalSize {
    cols: 200,
    rows: 50,
    pixel_width: 0,
    pixel_height: 0,
};
/// Text after the line number: ~55 columns for B2 (80-column terminals), ~175 for B2w.
const FILLER: &str = "the quick brown fox jumps over the lazy dog 0123456789";
const WIDE_FILLER: &str = "the quick brown fox jumps over the lazy dog 0123456789 the quick brown fox jumps over the lazy dog 0123456789 the quick brown fox jumps over the lazy dog 0123456789";

/// Starts `LOADED_SESSIONS` sessions that each print 12 000 coloured lines (the 10 000-line
/// scrollback overflows), then idle in `cat`; returns once all printed and the daemon settled.
async fn load_sessions(
    stack: &Stack<'_>,
    size: Option<TerminalSize>,
    filler: &str,
) -> Result<Vec<SessionInfo>> {
    let mut sessions = Vec::new();
    let mut marks = Vec::new();
    for n in 0..LOADED_SESSIONS {
        let done = stack.path(&format!("done-{n}"));
        let script = format!(
            "awk 'BEGIN{{for(i=1;i<=12000;i++) printf \"\\033[3%dmline %d\\033[0m {filler}\\n\", i%7+1, i}}'; \
             touch '{}'; exec cat",
            done.display()
        );
        sessions.push(stack.create_sized(&["sh", "-c", &script], size).await?);
        marks.push(done);
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while marks.iter().any(|done| !done.exists()) {
        if Instant::now() > deadline {
            bail!("sessions did not finish printing");
        }
        sleep(Duration::from_millis(100)).await;
    }
    sleep(Duration::from_secs(2)).await;
    Ok(sessions)
}

/// B2w: the same load in 200-column terminals, no client attached.
async fn idle_wide(stack: &mut Stack<'_>) -> Result<Metrics> {
    let _sessions = load_sessions(stack, Some(WIDE_SIZE), WIDE_FILLER).await?;
    let mut metrics = Metrics::new();
    metrics.insert(
        "b2w_daemon_rss_mb".into(),
        stack.daemon_usage()?.rss_bytes as f64 / MIB,
    );
    Ok(metrics)
}

async fn idle(stack: &mut Stack<'_>) -> Result<Metrics> {
    let mut metrics = Metrics::new();
    let sessions = load_sessions(stack, None, FILLER).await?;
    let detached = stack.daemon_usage()?;
    metrics.insert(
        "b2_daemon_rss_detached_mb".into(),
        detached.rss_bytes as f64 / MIB,
    );

    let shown: Vec<&SessionInfo> = sessions.iter().take(SHOWN_SESSIONS).collect();
    stack.launch_desktop(&shown, false, None)?;
    let started = Instant::now();
    while stack.clients().await? < 2 {
        stack.check_desktop()?;
        if started.elapsed() > Duration::from_secs(30) {
            bail!("the desktop never connected");
        }
        sleep(Duration::from_millis(100)).await;
    }
    sleep(SETTLE).await;
    stack.check_desktop()?;

    let desktop_pid = stack.desktop()?.pid();
    let desktop0 = proc::usage(desktop_pid)?;
    let daemon0 = stack.daemon_usage()?;
    metrics.insert("b1_desktop_rss_mb".into(), desktop0.rss_bytes as f64 / MIB);
    metrics.insert(
        "b1_desktop_footprint_mb".into(),
        desktop0.footprint_bytes as f64 / MIB,
    );
    metrics.insert("b2_daemon_rss_mb".into(), daemon0.rss_bytes as f64 / MIB);

    measure_cpu(stack, "b3", desktop0, daemon0, &mut metrics).await?;
    Ok(metrics)
}

/// The idle CPU window: samples (with `--sample`) and idles for [`CPU_WINDOW`], then records
/// `<prefix>_idle_cpu_pct` (daemon + desktop), each part and the wakeups, against the usage
/// read when the window opened.
async fn measure_cpu(
    stack: &mut Stack<'_>,
    prefix: &str,
    desktop0: proc::Usage,
    daemon0: proc::Usage,
    metrics: &mut Metrics,
) -> Result<()> {
    let desktop_pid = stack.desktop()?.pid();
    let samples = [
        stack.sample(desktop_pid, 10, &format!("{prefix}-idle-desktop")),
        stack.sample(stack.daemon.pid(), 10, &format!("{prefix}-idle-daemon")),
    ];
    let t0 = Instant::now();
    sleep(CPU_WINDOW).await;
    for mut sample in samples.into_iter().flatten() {
        let _ = sample.wait();
    }
    let elapsed = t0.elapsed().as_secs_f64();
    stack.check_desktop()?;
    let desktop1 = proc::usage(desktop_pid)?;
    let daemon1 = stack.daemon_usage()?;
    let pct = |a: &proc::Usage, b: &proc::Usage| (b.cpu - a.cpu).as_secs_f64() / elapsed * 100.0;
    let (desktop_pct, daemon_pct) = (pct(&desktop0, &desktop1), pct(&daemon0, &daemon1));
    metrics.insert(format!("{prefix}_desktop_cpu_pct"), desktop_pct);
    metrics.insert(format!("{prefix}_daemon_cpu_pct"), daemon_pct);
    metrics.insert(format!("{prefix}_idle_cpu_pct"), desktop_pct + daemon_pct);
    metrics.insert(
        format!("{prefix}_desktop_wakeups_per_s"),
        (desktop1.wakeups - desktop0.wakeups) as f64 / elapsed,
    );
    metrics.insert(
        format!("{prefix}_daemon_wakeups_per_s"),
        (daemon1.wakeups - daemon0.wakeups) as f64 / elapsed,
    );
    metrics.insert(
        format!("{prefix}_desktop_rss_mb"),
        desktop1.rss_bytes as f64 / MIB,
    );
    metrics.insert(
        format!("{prefix}_daemon_rss_mb"),
        daemon1.rss_bytes as f64 / MIB,
    );
    Ok(())
}

/// B3p's entry: the usage when the window opens, then [`measure_cpu`].
pub(crate) async fn idle_cpu(
    stack: &mut Stack<'_>,
    prefix: &str,
    metrics: &mut Metrics,
) -> Result<()> {
    let desktop0 = proc::usage(stack.desktop()?.pid())?;
    let daemon0 = stack.daemon_usage()?;
    measure_cpu(stack, prefix, desktop0, daemon0, metrics).await
}

const TYPING_KEYS: usize = 500;
const TYPING_INTERVAL_MS: u64 = 100;
pub(crate) const SCRIPT_START_DELAY_MS: u64 = 1500;
/// Spread of key times over one display frame at least (see `Script::jitter` in the desktop).
const KEY_JITTER_MS: u64 = 17;

async fn typing(stack: &mut Stack<'_>, key_interval_ms: Option<u64>) -> Result<Metrics> {
    let interval_ms = key_interval_ms.unwrap_or(TYPING_INTERVAL_MS);
    let session = stack.create(PROMPT_CAT).await?;
    let script = seed::typing_script(
        TYPING_KEYS,
        interval_ms,
        SCRIPT_START_DELAY_MS,
        KEY_JITTER_MS,
    );
    stack.launch_desktop(&[&session], true, Some(&script))?;
    let budget = Duration::from_millis(TYPING_KEYS as u64 * interval_ms + 30_000);
    stack
        .wait_trace("bench_start", budget, |e| is_event(e, "bench_start"))
        .await?;
    let desktop_pid = stack.desktop()?.pid();
    let samples = [
        stack.sample(desktop_pid, 8, "typing-desktop"),
        stack.sample(stack.daemon.pid(), 8, "typing-daemon"),
    ];
    stack
        .wait_trace("bench_done", budget, |e| is_event(e, "bench_done"))
        .await?;
    for mut sample in samples.into_iter().flatten() {
        let _ = sample.wait();
    }
    // The last echoes arrive after the last key was sent.
    sleep(Duration::from_secs(1)).await;
    stack.check_desktop()?;
    let events = stack.trace();
    let totals = key_totals_ms(&events, None);
    if totals.len() < TYPING_KEYS * 95 / 100 {
        bail!(
            "only {} of {TYPING_KEYS} keystrokes were painted",
            totals.len()
        );
    }
    let mut metrics = Metrics::new();
    // B4b: key handled to the end of the paint that shows the echo (as before, B4).
    record_latency(&mut metrics, "b4b", &totals);
    // B4a: the same without the wait for the display tick (`echo_to_prepaint`).
    record_latency(&mut metrics, "b4a", &key_hutan_ms(&events));
    record_stages(&mut metrics, "b4", &events, None);
    metrics.insert("b4_window_active".into(), window_active(&events));
    let interval_ms = display_refresh_interval_ms(&events)?;
    metrics.insert(REFRESH_INTERVAL_MS.into(), interval_ms);
    metrics.insert("b4_refresh_hz".into(), 1000.0 / interval_ms);
    // A window that is not being presented (inactive, occluded, on a sleeping display) paints only
    // when something else wakes it, e.g. the next key, so the echo waits ~a key interval for its
    // paint instead of one tick. Such a run measures the machine, not Hutan: the harness repeats
    // it, and fails rather than reporting it.
    let waits = key_stage_ms(&events, "echo_to_prepaint_us", None);
    let stalled = metrics.get("b4_window_active") != Some(&1.0)
        || percentile(&waits, 50.0).is_some_and(|p50| p50 > 3.0 * interval_ms);
    metrics.insert(DISPLAY_STALLED.into(), f64::from(u8::from(stalled)));
    Ok(metrics)
}

/// The refresh interval (ms) of the display the window was on, as GPUI reported it when the key
/// script began and when it ended. Fails when the platform did not report one or the window
/// moved to another display in between: B4b has no defined limit then.
pub(crate) fn display_refresh_interval_ms(events: &[Value]) -> Result<f64> {
    let at = |name: &str| {
        events
            .iter()
            .find(|e| is_event(e, name))
            .and_then(|e| e["refresh_interval_us"].as_u64())
    };
    let (Some(start), Some(done)) = (at("bench_start"), at("bench_done")) else {
        bail!("the platform did not report the display's refresh interval");
    };
    if start != done {
        bail!("the window moved between displays during the run ({start} us, then {done} us)");
    }
    Ok(start as f64 / 1000.0)
}

/// 1.0 when the desktop window was the active (key) window when the key script began. GPUI
/// caps inactive windows at 30 fps, so latency numbers are only representative when it was.
pub(crate) fn window_active(events: &[Value]) -> f64 {
    let active = events
        .iter()
        .find(|e| is_event(e, "bench_start"))
        .is_some_and(|e| e["window_active"] == true);
    f64::from(u8::from(active))
}

const FLOOD_KEYS: usize = 600;
/// 20 keys/s with jitter: a fast typist. Faster than one key per ack round trip (about two
/// display frames) makes echoes queue behind acks (docs/perf.md, B5).
const FLOOD_INTERVAL_MS: u64 = 50;
const FLOOD_DEADLINE: Duration = Duration::from_secs(600);
/// Output of one `head -c 150000000 | base64`: 76 characters + newline per 57 bytes ≈ 203 MB.
/// The pipeline takes ~2 s (the daemon parses ~100 MB/s whatever the display does), too short
/// to type through, so the flood repeats `FLOOD_ROUNDS` times (≈ 1.2 GB) back to back.
const FLOOD_ROUNDS: usize = 6;
const FLOOD_COMMAND: &str = "read go; for i in $(seq {rounds}); do head -c 150000000 /dev/urandom | base64; done; touch '{done}'; exec cat";

async fn flood(stack: &mut Stack<'_>, key_interval_ms: Option<u64>) -> Result<Metrics> {
    let interval_ms = key_interval_ms.unwrap_or(FLOOD_INTERVAL_MS);
    let done = stack.path("flood-done");
    let typing = stack.create(PROMPT_CAT).await?;
    let script = FLOOD_COMMAND
        .replace("{rounds}", &FLOOD_ROUNDS.to_string())
        .replace("{done}", &done.display().to_string());
    // The flood session also prints a prompt so its pane has text before the flood starts.
    let flooded = stack
        .create(&["sh", "-c", &format!("printf 'flood> '; {script}")])
        .await?;
    let keys = seed::typing_script(
        FLOOD_KEYS,
        interval_ms,
        SCRIPT_START_DELAY_MS,
        KEY_JITTER_MS,
    );
    stack.launch_desktop(&[&typing, &flooded], true, Some(&keys))?;
    stack
        .wait_trace("bench_start", Duration::from_secs(60), |e| {
            is_event(e, "bench_start")
        })
        .await?;

    let baseline = stack.daemon_usage()?.rss_bytes;
    let flood_start = now_us();
    stack.type_into(&flooded, "go\n").await?;
    let mut peak = baseline;
    let deadline = Instant::now() + FLOOD_DEADLINE;
    while !done.exists() {
        stack.check_desktop()?;
        if Instant::now() > deadline {
            bail!("the flood did not finish in {FLOOD_DEADLINE:?}");
        }
        peak = peak.max(stack.daemon_usage()?.rss_bytes);
        sleep(Duration::from_millis(100)).await;
    }
    let flood_end = now_us();
    peak = peak.max(stack.daemon_usage()?.rss_bytes);
    stack
        .wait_trace(
            "bench_done",
            Duration::from_millis(FLOOD_KEYS as u64 * interval_ms) + Duration::from_secs(60),
            |e| is_event(e, "bench_done"),
        )
        .await?;
    sleep(Duration::from_secs(1)).await;
    stack.check_desktop()?;

    let events = stack.trace();
    let during = key_totals_ms(&events, Some((flood_start, flood_end)));
    let all = key_totals_ms(&events, None);
    if during.len() < 100 {
        bail!(
            "only {} keystrokes were painted while the flood ran ({:.1} s); the flood was too short to measure",
            during.len(),
            (flood_end - flood_start) as f64 / 1e6
        );
    }
    let mut metrics = Metrics::new();
    record_latency(&mut metrics, "b5", &during);
    record_stages(&mut metrics, "b5", &events, Some((flood_start, flood_end)));
    // Paints per second of the busiest view while flooded: the display's frame rate.
    let rates: Vec<f64> = events
        .iter()
        .filter(|e| is_event(e, "paints"))
        .filter(|e| {
            e["t_us"]
                .as_u64()
                .is_some_and(|t| flood_start <= t && t <= flood_end)
        })
        .filter_map(|e| Some(e["n"].as_f64()? / (e["span_us"].as_f64()? / 1e6)))
        .collect();
    if let Some(rate) = percentile(&rates, 100.0) {
        metrics.insert("b5_paints_per_s_max".into(), rate);
    }
    record_latency(&mut metrics, "b5_all", &all);
    metrics.insert(
        "b5_daemon_growth_mb".into(),
        peak.saturating_sub(baseline) as f64 / MIB,
    );
    metrics.insert("b5_daemon_baseline_mb".into(), baseline as f64 / MIB);
    metrics.insert("b5_daemon_peak_mb".into(), peak as f64 / MIB);
    metrics.insert(
        "b5_flood_seconds".into(),
        (flood_end - flood_start) as f64 / 1e6,
    );
    metrics.insert("b5_keys_sent".into(), FLOOD_KEYS as f64);
    Ok(metrics)
}

async fn cold_start(stack: &mut Stack<'_>) -> Result<Metrics> {
    let mut sessions = Vec::new();
    for _ in 0..SHOWN_SESSIONS {
        sessions.push(stack.create(PROMPT_CAT).await?);
    }
    // Let the prompts reach the daemon's screens before the clock starts.
    sleep(Duration::from_secs(1)).await;
    let shown: Vec<&SessionInfo> = sessions.iter().collect();
    let spawned = now_us();
    stack.launch_desktop(&shown, true, None)?;
    let painted = stack
        .wait_trace("the first terminal paint", Duration::from_secs(30), |e| {
            e["ev"] == "startup" && e["stage"] == "first_terminal_painted"
        })
        .await?;
    stack.check_desktop()?;
    let events = stack.trace();
    let since = |t: u64| t.saturating_sub(spawned) as f64 / 1000.0;
    let mut metrics = Metrics::new();
    metrics.insert(
        "b6_cold_start_ms".into(),
        since(painted["t_us"].as_u64().unwrap_or(0)),
    );
    for stage in ["main", "window_opened", "first_frame", "connected"] {
        if let Some(t) = startup_us(&events, stage) {
            metrics.insert(format!("b6_{stage}_ms"), since(t));
        }
    }
    Ok(metrics)
}
