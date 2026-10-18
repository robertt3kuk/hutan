//! Index a generated corpus and report what it costs (`docs/history-index.md` § Performance).
//!
//! ```text
//! cargo run --release -p hutan-index --example perf -- [--messages 100000] [--idle-secs 10]
//! ```
//!
//! The corpus is synthetic Claude Code transcripts in a temporary directory: about a third user
//! and assistant text, the rest tool calls and results of 0.2–3 KiB, with words drawn from a
//! Zipf-ish vocabulary plus paths, flags, and commands. Nothing outside the temporary directory
//! is read or written.
#![allow(clippy::unwrap_used, clippy::expect_used)] // a measurement tool: fail loudly

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use hutan_index::{IndexConfig, Indexer, Role, SearchQuery, Source, process_cpu_micros};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }
}

fn vocabulary(rng: &mut Rng) -> Vec<String> {
    let syllables = [
        "ka", "lo", "mi", "ne", "ra", "su", "ta", "vo", "xe", "zi", "pre", "con", "tra", "ing",
        "ment", "der", "sta", "ble", "cor", "pus",
    ];
    (0..6000)
        .map(|_| {
            let parts = 1 + rng.below(3);
            (0..parts)
                .map(|_| syllables[rng.below(syllables.len())])
                .collect()
        })
        .collect()
}

const COMMANDS: &[&str] = &[
    "cargo test -p daemon --locked",
    "cargo clippy --workspace --all-targets",
    "git status --short",
    "just check",
    "rg -n parse_line crates/index/src",
    "kubectl get pods -n staging",
];
const PATHS: &[&str] = &[
    "crates/daemon/src/registry.rs",
    "crates/index/src/scan.rs",
    "crates/terminal/src/emulator.rs",
    "docs/architecture.md",
    "src/main.rs",
];

fn words(rng: &mut Rng, vocab: &[String], count: usize) -> String {
    let mut out = String::new();
    for i in 0..count {
        if i > 0 {
            out.push(' ');
        }
        // Zipf-ish: squaring the uniform draw favours the head of the vocabulary.
        let r = rng.below(1000);
        let index = r * r * vocab.len() / 1_000_000;
        out.push_str(&vocab[index.min(vocab.len() - 1)]);
        if rng.below(40) == 0 {
            let _ = write!(out, " {}", PATHS[rng.below(PATHS.len())]);
        }
        if rng.below(60) == 0 {
            let _ = write!(out, " `{}`", COMMANDS[rng.below(COMMANDS.len())]);
        }
    }
    out
}

fn escape(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

fn line(kind: &str, session: &str, ts: usize, content: &str) -> String {
    format!(
        r#"{{"type":"{kind}","timestamp":"2026-10-{:02}T{:02}:{:02}:00.000Z","cwd":"/work/proj","sessionId":"{session}","message":{{"role":"{kind}","content":{content}}}}}"#,
        1 + ts / 1440 % 28,
        ts / 60 % 24,
        ts % 60
    )
}

fn generate(root: &Path, messages: usize) -> (usize, u64) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let vocab = vocabulary(&mut rng);
    let per_file = 100;
    let files = messages.div_ceil(per_file);
    let mut bytes = 0_u64;
    let mut written = 0;
    for file_index in 0..files {
        let session = format!("{file_index:08x}-0000-4000-8000-000000000000");
        let dir = root.join(format!("-work-proj{}", file_index % 40));
        fs::create_dir_all(&dir).unwrap();
        let mut file = std::io::BufWriter::new(
            fs::File::create(dir.join(format!("{session}.jsonl"))).unwrap(),
        );
        for n in 0..per_file.min(messages - written) {
            let ts = file_index * 7 + n;
            let text = match n % 5 {
                0 => {
                    let count = 20 + rng.below(40);
                    let body = words(&mut rng, &vocab, count);
                    line(
                        "user",
                        &session,
                        ts,
                        &format!(r#"[{{"type":"text","text":{}}}]"#, escape(&body)),
                    )
                }
                1 | 3 => {
                    let count = 60 + rng.below(140);
                    let body = words(&mut rng, &vocab, count);
                    line(
                        "assistant",
                        &session,
                        ts,
                        &format!(r#"[{{"type":"text","text":{}}}]"#, escape(&body)),
                    )
                }
                2 => {
                    let command = COMMANDS[rng.below(COMMANDS.len())];
                    line(
                        "assistant",
                        &session,
                        ts,
                        &format!(
                            r#"[{{"type":"tool_use","id":"t","name":"Bash","input":{{"command":{},"description":{}}}}}]"#,
                            escape(command),
                            escape(&words(&mut rng, &vocab, 6))
                        ),
                    )
                }
                _ => {
                    let count = 40 + rng.below(400);
                    let body = words(&mut rng, &vocab, count);
                    line(
                        "user",
                        &session,
                        ts,
                        &format!(
                            r#"[{{"type":"tool_result","tool_use_id":"t","content":{}}}]"#,
                            escape(&body)
                        ),
                    )
                }
            };
            bytes += text.len() as u64 + 1;
            writeln!(file, "{text}").unwrap();
            written += 1;
        }
        file.flush().unwrap();
    }
    (files, bytes)
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn main() {
    let mut messages = 100_000;
    let mut idle_secs = 10;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--messages" => messages = args.next().and_then(|v| v.parse().ok()).unwrap_or(messages),
            "--idle-secs" => {
                idle_secs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(idle_secs)
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let corpus = tmp.path().join("claude/projects");
    let started = Instant::now();
    let (files, corpus_bytes) = generate(&corpus, messages);
    println!(
        "corpus: {messages} messages in {files} files, {:.1} MiB (generated in {:.1}s)",
        corpus_bytes as f64 / 1_048_576.0,
        started.elapsed().as_secs_f64()
    );

    let db = tmp.path().join("home/index.sqlite");
    let cpu0 = process_cpu_micros();
    let wall = Instant::now();
    let indexer = Indexer::start(IndexConfig {
        db_path: db.clone(),
        range_days: 0,
        roots: vec![(Source::Claude, corpus.clone())],
        hutan: false,
    })
    .unwrap();
    let handle = indexer.handle();
    handle.flush().unwrap(); // returns after the startup pass
    let elapsed = wall.elapsed();
    let cpu = Duration::from_micros(process_cpu_micros() - cpu0);
    let status = handle.status().unwrap();
    let rows: u64 = status.sources.iter().map(|s| s.rows).sum();
    println!(
        "index: {:.1}s wall, {:.1}s cpu, {rows} rows, {:.0} messages/s, db {:.1} MiB ({:.0}% of the corpus)",
        elapsed.as_secs_f64(),
        cpu.as_secs_f64(),
        messages as f64 / elapsed.as_secs_f64(),
        status.db_bytes as f64 / 1_048_576.0,
        status.db_bytes as f64 * 100.0 / corpus_bytes as f64
    );
    println!(
        "scan: {} files opened, {:.1} MiB read, {} re-indexed",
        handle.metrics().files_opened(),
        handle.metrics().bytes_read() as f64 / 1_048_576.0,
        handle.metrics().reindexed()
    );

    // Searches: identifiers, common words, rare words, phrases.
    let queries = [
        "cargo test -p daemon",
        "crates/index/src/scan.rs",
        "kubectl get pods",
        "parse_line",
        "ka lo",
        "preconstra",
        "zi vo xe",
        "\"cor pus\"",
    ];
    let mut times = Vec::new();
    for text in queries {
        let mut own = Vec::new();
        let mut found = 0;
        for _ in 0..10 {
            let t = Instant::now();
            let hits = handle
                .search(&SearchQuery {
                    text: text.to_owned(),
                    limit: 20,
                    ..SearchQuery::default()
                })
                .unwrap();
            own.push(t.elapsed());
            found = hits.len();
        }
        own.sort();
        println!(
            "  {text:<28} {:>6.1} ms median, {found} hit(s)",
            percentile(&own, 0.5).as_secs_f64() * 1e3
        );
        times.extend(own);
    }
    times.sort();
    println!(
        "search: p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms over {} queries",
        percentile(&times, 0.5).as_secs_f64() * 1e3,
        percentile(&times, 0.95).as_secs_f64() * 1e3,
        times.last().map_or(0.0, |t| t.as_secs_f64() * 1e3),
        times.len()
    );
    let t = Instant::now();
    let hits = handle
        .search(&SearchQuery {
            text: "cargo test -p daemon".to_owned(),
            limit: 20,
            roles: vec![Role::Tool],
            ..SearchQuery::default()
        })
        .unwrap();
    println!(
        "search (role filter): {:.1} ms, {} hits",
        t.elapsed().as_secs_f64() * 1e3,
        hits.len()
    );

    // The session catalog: every session of the corpus with its summary.
    let mut catalog_times = Vec::new();
    let mut sessions = 0;
    for _ in 0..5 {
        let t = Instant::now();
        sessions = handle.catalog().unwrap().len();
        catalog_times.push(t.elapsed());
    }
    catalog_times.sort();
    println!(
        "catalog: {sessions} sessions, median {:.1} ms (max {:.1} ms)",
        catalog_times[catalog_times.len() / 2].as_secs_f64() * 1e3,
        catalog_times[catalog_times.len() - 1].as_secs_f64() * 1e3
    );

    // One appended line becomes searchable.
    let target = corpus.join("-work-proj0/00000000-0000-4000-8000-000000000000.jsonl");
    let before = handle.metrics().bytes_read();
    let t = Instant::now();
    let mut file = fs::OpenOptions::new().append(true).open(&target).unwrap();
    writeln!(
        file,
        "{}",
        line(
            "user",
            "00000000",
            1,
            r#"[{"type":"text","text":"zyzzyva appended marker"}]"#
        )
    )
    .unwrap();
    drop(file);
    let seen = loop {
        let hits = handle
            .search(&SearchQuery {
                text: "zyzzyva".to_owned(),
                limit: 5,
                ..SearchQuery::default()
            })
            .unwrap();
        if !hits.is_empty() || t.elapsed() > Duration::from_secs(10) {
            break t.elapsed();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    println!(
        "append: searchable after {:.0} ms; {} bytes read for it",
        seen.as_secs_f64() * 1e3,
        handle.metrics().bytes_read() - before
    );

    // Idle: no events, no timers.
    std::thread::sleep(Duration::from_secs(2));
    let cpu0 = process_cpu_micros();
    let wall = Instant::now();
    std::thread::sleep(Duration::from_secs(idle_secs));
    let cpu = process_cpu_micros() - cpu0;
    println!(
        "idle: {} ms cpu over {:.1}s wall = {:.3}% of one core",
        cpu / 1000,
        wall.elapsed().as_secs_f64(),
        cpu as f64 / 1e6 / wall.elapsed().as_secs_f64() * 100.0
    );
    drop(indexer);
}
