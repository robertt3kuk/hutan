//! Snapshot cost of the checkpoint engine on a generated 10 000-file repository (docs/perf.md §
//! Checkpoints). Not part of the gate:
//!
//! ```text
//! cargo run -p hutan-daemon --release --example checkpoint_bench [-- <runs>]
//! ```
//!
//! The repository mirrors the spike's (`docs/spikes/2026-10-21-checkpoints.md` §4.1): 100
//! directories × 100 `.rs` files of 3–8 KiB, 20 binaries of 1 MiB, an ignored `node_modules/`
//! of 5 000 small files; committed and `git gc`'d. It lives in a temporary directory and is
//! removed afterwards; the user's git configuration is not read. `RUST_LOG=hutan_daemon=debug`
//! prints the time of every git step.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use hutan_daemon::checkpoints::{Checkpoints, Config, Kind, Label, Snapshot};

/// xorshift64*: deterministic content without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low)
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 8);
        while out.len() < len {
            out.extend_from_slice(&self.next().to_le_bytes());
        }
        out.truncate(len);
        out
    }

    fn source(&mut self) -> String {
        let lines = self.range(60, 150);
        let mut text = String::new();
        for i in 0..lines {
            let width = self.range(20, 90) as usize;
            text.push_str(&format!("    let v{i} = \"{}\";\n", "x".repeat(width)));
        }
        text
    }
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Bench", "-c", "user.email=bench@localhost"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .context("run git")?;
    if !output.status.success() {
        bail!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Loose plus packed object bytes (`count-objects -v`, KiB).
fn object_kib(root: &Path) -> anyhow::Result<u64> {
    let counts = git(root, &["count-objects", "-v"])?;
    let mut total = 0;
    for line in counts.lines() {
        if let Some(kib) = line
            .strip_prefix("size: ")
            .or_else(|| line.strip_prefix("size-pack: "))
        {
            total += kib.trim().parse::<u64>()?;
        }
    }
    Ok(total)
}

fn generate(root: &Path, rng: &mut Rng) -> anyhow::Result<()> {
    git(
        root.parent().context("parent")?,
        &["init", "-q", "-b", "main", "repo"],
    )?;
    for d in 0..100 {
        let dir = root.join(format!("src/d{d:03}"));
        fs::create_dir_all(&dir)?;
        for f in 0..100 {
            fs::write(dir.join(format!("f{f:03}.rs")), rng.source())?;
        }
    }
    fs::create_dir_all(root.join("assets"))?;
    for b in 0..20 {
        fs::write(
            root.join(format!("assets/b{b:02}.bin")),
            rng.bytes(1024 * 1024),
        )?;
    }
    fs::write(root.join(".gitignore"), "node_modules/\ntarget/\n.env\n")?;
    for n in 0..5_000 {
        let dir = root.join(format!("node_modules/p{:02}", n / 100));
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(format!("m{n}.js")),
            format!("module.exports = {n};\n"),
        )?;
    }
    fs::write(root.join(".env"), "SECRET=1\n")?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-q", "-m", "generated"])?;
    git(root, &["gc", "-q"])?;
    // The user's own `git status` once, so the user's index is stat-clean as in daily use.
    git(root, &["status", "--porcelain"])?;
    Ok(())
}

/// Rewrites `count` (≤ 10 000) distinct source files and adds `new` files, all with fresh
/// content.
fn edit(root: &Path, rng: &mut Rng, count: u64, new: u64, round: usize) -> anyhow::Result<()> {
    for i in 0..count {
        let path = root.join(format!(
            "src/d{:03}/f{:03}.rs",
            i % 100,
            (i / 100 + round as u64 * 3) % 100
        ));
        fs::write(path, rng.source())?;
    }
    for i in 0..new {
        let dir = root.join(format!("src/new{round}"));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(format!("n{i}.rs")), rng.source())?;
    }
    Ok(())
}

struct Row {
    name: &'static str,
    times: Vec<Duration>,
    growth_kib: Vec<u64>,
    hashed: Vec<usize>,
    skipped: usize,
}

fn median<T: Copy + Ord>(values: &[T]) -> T {
    let mut sorted = values.to_vec();
    sorted.sort();
    sorted[sorted.len() / 2]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let runs: usize = std::env::args().nth(1).map_or(Ok(5), |a| a.parse())?;
    let dir = tempfile::tempdir()?;
    let base = fs::canonicalize(dir.path())?;
    let root = base.join("repo");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let generated = Instant::now();
    generate(&root, &mut rng)?;
    eprintln!("generated in {:.1} s", generated.elapsed().as_secs_f64());

    let mut config = Config::new(base.join("store"), None);
    config.git_env = BTreeMap::from([
        ("GIT_CONFIG_GLOBAL".to_owned(), "/dev/null".to_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ]);
    let engine = Checkpoints::new(config);
    let mut turn = 0usize;
    let mut snap = |engine: Checkpoints, root: PathBuf| {
        turn += 1;
        let label = Label::new("bench", turn.to_string(), Kind::Start);
        async move {
            let objects = object_kib(&root)?;
            let started = Instant::now();
            let snapshot: Snapshot = engine.snapshot(&root, label).await?;
            let took = started.elapsed();
            Ok::<_, anyhow::Error>((took, object_kib(&root)?.saturating_sub(objects), snapshot))
        }
    };

    let mut status = Vec::new();
    for _ in 0..runs {
        let started = Instant::now();
        git(&root, &["status", "--porcelain"])?;
        status.push(started.elapsed());
    }

    let (first, first_growth, first_snapshot) = snap(engine.clone(), root.clone()).await?;
    let mut rows = Vec::new();
    // (name, files rewritten, files added, a new 5 MiB binary, every source file touched)
    let scenarios: [(&'static str, u64, u64, bool, bool); 5] = [
        ("unchanged", 0, 0, false, false),
        ("3 edits", 3, 0, false, false),
        (
            "250 edits (200 modified + 50 new files)",
            200,
            50,
            false,
            false,
        ),
        ("one new 5 MiB binary", 0, 0, true, false),
        ("10 000 files touched, same content", 0, 0, false, true),
    ];
    for (name, count, new, binary, touch) in scenarios {
        let mut row = Row {
            name,
            times: Vec::new(),
            growth_kib: Vec::new(),
            hashed: Vec::new(),
            skipped: 0,
        };
        for round in 0..runs {
            edit(&root, &mut rng, count, new, round)?;
            if binary {
                fs::write(
                    root.join(format!("assets/new{round}.bin")),
                    rng.bytes(5 * 1024 * 1024),
                )?;
            }
            if touch {
                let now = std::time::SystemTime::now();
                for d in 0..100 {
                    for f in 0..100 {
                        let path = root.join(format!("src/d{d:03}/f{f:03}.rs"));
                        fs::File::options()
                            .append(true)
                            .open(path)?
                            .set_modified(now)?;
                    }
                }
            }
            let (took, growth, snapshot) = snap(engine.clone(), root.clone()).await?;
            row.times.push(took);
            row.growth_kib.push(growth);
            row.hashed.push(snapshot.files_hashed);
            row.skipped = snapshot.skipped.len();
        }
        rows.push(row);
    }

    println!("| Scenario | Median of {runs} | Runs (ms) | Files hashed | Object growth (median) |");
    println!("|---|---|---|---|---|");
    println!(
        "| `git status --porcelain` (reference) | {:.0} ms | {} | — | — |",
        median(&status).as_secs_f64() * 1e3,
        status
            .iter()
            .map(|t| format!("{:.0}", t.as_secs_f64() * 1e3))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "| first snapshot (seed from the user's index) | {:.0} ms | 1 run | {} | {} KiB |",
        first.as_secs_f64() * 1e3,
        first_snapshot.files_hashed,
        first_growth
    );
    for row in rows {
        println!(
            "| {} | **{:.0} ms** | {} | {} | {} KiB |",
            row.name,
            median(&row.times).as_secs_f64() * 1e3,
            row.times
                .iter()
                .map(|t| format!("{:.0}", t.as_secs_f64() * 1e3))
                .collect::<Vec<_>>()
                .join(", "),
            median(&row.hashed),
            median(&row.growth_kib),
        );
        if row.skipped > 0 {
            println!("| ↳ skipped in the last run | | | {} | |", row.skipped);
        }
    }
    Ok(())
}
