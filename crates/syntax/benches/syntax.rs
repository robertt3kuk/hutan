//! Timings for `docs/perf.md` (Editor: syntax). Run `cargo bench -p hutan-syntax --bench syntax`.
//! No harness crate: `Instant`, median of several runs.
#![allow(clippy::expect_used)] // a benchmark that cannot run should stop loudly

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hutan_syntax::{Language, Snapshot, Syntax, TextEdit};

const MB: usize = 1_000_000;

/// Realistic-looking Rust of at least `bytes`: structs, impls, matches, closures, comments,
/// strings and attributes, every item distinct.
fn rust_source(bytes: usize) -> String {
    let mut out = String::with_capacity(bytes + 4096);
    let mut i = 0usize;
    while out.len() < bytes {
        out.push_str(&format!(
            r#"/// Item number {i}: a record with a few fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Record{i}<T: Clone> {{
    id: u64,
    name: String,
    values: Vec<T>,
    parent: Option<Box<Record{i}<T>>>,
}}

impl<T: Clone + std::fmt::Debug> Record{i}<T> {{
    pub const LIMIT: usize = {limit};

    pub fn new(id: u64, name: &str) -> Self {{
        Self {{ id, name: name.to_string(), values: Vec::new(), parent: None }}
    }}

    /// Adds `value` unless the record is full.
    pub fn push(&mut self, value: T) -> Result<usize, String> {{
        if self.values.len() >= Self::LIMIT {{
            return Err(format!("record {{}} is full ({{}} items)", self.name, Self::LIMIT));
        }}
        self.values.push(value);
        Ok(self.values.len())
    }}

    fn describe(&self, depth: u32) -> String {{
        let mut text = String::new();
        for (index, value) in self.values.iter().enumerate() {{
            match index % 3 {{
                0 => text.push_str(&format!("{{:?}}\n", value)),
                1 if depth > 2 => text.push_str("deep"),
                _ => {{ /* skipped */ }}
            }}
        }}
        let total: u64 = self.values.iter().map(|_| 1u64).sum::<u64>() + self.id * {i};
        text + &total.to_string()
    }}
}}

"#,
            limit = 8 + i % 24
        ));
        i += 1;
    }
    out
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> Duration {
    // One untimed run first so caches and the language's queries are warm.
    std::hint::black_box(f());
    median(
        (0..runs)
            .map(|_| {
                let started = Instant::now();
                std::hint::black_box(f());
                started.elapsed()
            })
            .collect(),
    )
}

fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1e3)
}

fn us(d: Duration) -> String {
    format!("{:.1} µs", d.as_secs_f64() * 1e6)
}

fn rss_kb() -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

/// The byte range of `lines` lines starting at the line containing `at`.
fn viewport(text: &str, at: usize, lines: usize) -> std::ops::Range<usize> {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let mut end = start;
    for _ in 0..lines {
        match text[end..].find('\n') {
            Some(i) => end += i + 1,
            None => {
                end = text.len();
                break;
            }
        }
    }
    start..end
}

fn rust_1mb() {
    println!("## Rust, {} bytes", rust_source(MB).len());
    let source = rust_source(MB);
    let len = source.len();

    let started = Instant::now();
    let first = Snapshot::parse(Language::Rust, String::new(), 0).expect("load");
    println!(
        "language load (grammar + query compile, first use): {}",
        ms(started.elapsed())
    );
    drop(first);

    let shared: Arc<str> = Arc::from(source.as_str());
    let before = rss_kb();
    let snapshot = Snapshot::parse(Language::Rust, Arc::clone(&shared), 0).expect("parse");
    let after = rss_kb();
    if let (Some(before), Some(after)) = (before, after) {
        println!(
            "RSS growth of one tree over the shared 1 MB text: {:.1} MB (an upper bound: includes malloc slack from the parse's temporary stacks)",
            (after as f64 - before as f64) / 1024.0
        );
    }
    // Several trees held at once separate the retained size from malloc slack.
    let mut held = vec![snapshot];
    let mut last = rss_kb().unwrap_or(0);
    for n in 2..=4 {
        held.push(Snapshot::parse(Language::Rust, Arc::clone(&shared), 0).expect("parse"));
        let now = rss_kb().unwrap_or(0);
        println!(
            "  tree {n} held: RSS +{:.1} MB",
            (now as f64 - last as f64) / 1024.0
        );
        last = now;
    }
    let snapshot = held.swap_remove(0);
    drop(held);
    println!("has errors: {}", snapshot.has_errors());

    for (name, at) in [("start", 0), ("middle", len / 2), ("end", len - 4000)] {
        let range = viewport(&source, at, 60);
        let t = time(51, || snapshot.highlights(range.clone()));
        println!(
            "highlights, 60 lines at the {name} ({} bytes, {} spans): {}",
            range.len(),
            snapshot.highlights(range.clone()).len(),
            us(t)
        );
    }
    let range = viewport(&source, len / 2, 200);
    println!(
        "highlights, 200 lines in the middle: {}",
        us(time(21, || snapshot.highlights(range.clone())))
    );
    println!(
        "highlights, whole file: {}",
        ms(time(5, || snapshot.highlights(0..len)))
    );
    let range = viewport(&source, len / 2, 60);
    println!(
        "folds, 60 lines in the middle: {}",
        us(time(51, || snapshot.folds(range.clone())))
    );
    println!(
        "folds, whole file: {}",
        ms(time(5, || snapshot.folds(0..len)))
    );
    let mid = len / 2;
    let mid = (mid..).find(|i| source.is_char_boundary(*i)).unwrap_or(mid);
    println!("node_at: {}", us(time(201, || snapshot.node_at(mid..mid))));
    println!(
        "expand_selection: {}",
        us(time(201, || snapshot.expand_selection(mid..mid)))
    );
    println!(
        "bracket_match: {}",
        us(time(201, || snapshot.bracket_match(mid)))
    );
    println!(
        "indent_level_after: {}",
        us(time(201, || snapshot.indent_level_after(mid)))
    );

    // Incremental reparse after typing one character in the middle of the file.
    let text_at = (mid..)
        .find(|i| source[*i..].starts_with("Self"))
        .unwrap_or(mid);
    let mut edited = source.clone();
    edited.insert(text_at, 'x');
    let edit = TextEdit::new(source.as_str(), text_at..text_at, "x").expect("edit");
    let incremental = time(21, || {
        snapshot
            .reparse(&[edit], edited.clone(), 1)
            .expect("reparse")
    });
    let clone_edited = time(21, || edited.clone());
    println!(
        "incremental reparse, one character typed in the middle: {} (of which cloning the String: {})",
        ms(incremental),
        ms(clone_edited)
    );
    // A Syntax handle with an O(1) text snapshot: what the editor pays on the calling thread per key.
    let shared: Arc<str> = Arc::from(edited.as_str());
    let syntax = Syntax::new(Language::Rust).expect("syntax");
    syntax.reparse(0, Arc::<str>::from(source.as_str()));
    syntax
        .wait_for(0, Duration::from_secs(60))
        .expect("first tree");
    let started = Instant::now();
    syntax.edit(1, edit);
    syntax.reparse(1, shared);
    let scheduled = started.elapsed();
    syntax.wait_for(1, Duration::from_secs(60)).expect("tree");
    let landed = started.elapsed();
    println!(
        "Syntax::edit + reparse (call returns): {}; until the new tree is published: {}; last parse on the worker: {}",
        us(scheduled),
        ms(landed),
        ms(syntax.stats().last_parse)
    );
    let range = viewport(&source, mid, 60);
    println!(
        "Syntax::highlights (view + 60 lines) on a current tree: {}",
        us(time(51, || syntax.highlights(range.clone())))
    );
    // Typing: the same call while the tree is one edit behind (old tree queried, spans mapped).
    let behind = Syntax::new(Language::Rust).expect("syntax");
    behind.reparse(0, Arc::<str>::from(source.as_str()));
    behind
        .wait_for(0, Duration::from_secs(60))
        .expect("first tree");
    behind.edit(1, edit);
    println!(
        "Syntax::highlights, 60 lines, tree one edit behind: {}",
        us(time(51, || behind.highlights(range.clone())))
    );
}

fn cancellation() {
    // A parse that takes long enough to interrupt, then a newer version.
    let big = rust_source(20 * MB);
    let syntax = Syntax::new(Language::Rust).expect("syntax");
    syntax.reparse(1, big.clone());
    std::thread::sleep(Duration::from_millis(60));
    let started = Instant::now();
    syntax.reparse(2, "fn small() {}".to_string());
    let tree = syntax
        .wait_for(2, Duration::from_secs(120))
        .expect("version 2");
    let stats = syntax.stats();
    println!(
        "\n## Cancellation\nnewer version submitted 60 ms into the parse of a {} MB file: new tree published after {} (cancelled: {}, completed: {}, version {})",
        big.len() / MB,
        ms(started.elapsed()),
        stats.cancelled,
        stats.completed,
        tree.version()
    );
}

/// Parse and viewport highlight of each language on its test fixture repeated to ~1 MB.
fn per_language() {
    println!("\n## Languages (the test fixture repeated to ~1 MB)");
    println!(
        "| Language | bytes | errors | full parse | highlights, 60 lines (middle) | language load |"
    );
    println!("|---|---|---|---|---|---|");
    for language in Language::compiled() {
        let name = match language {
            Language::Rust => "rust.rs",
            Language::TypeScript => "typescript.ts",
            Language::Tsx => "tsx.tsx",
            Language::JavaScript => "javascript.js",
            Language::Python => "python.py",
            Language::Go => "go.go",
            Language::Json => "json.json",
            Language::Toml => "toml.toml",
            Language::Yaml => "yaml.yaml",
            Language::Markdown => "markdown.md",
            Language::Bash => "bash.sh",
            Language::Html => "html.html",
            Language::Css => "css.css",
            Language::C => "c.c",
            Language::Cpp => "cpp.cpp",
        };
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/it/fixtures")
            .join(name);
        let mut unit = std::fs::read_to_string(path).expect("fixture");
        if language == Language::Yaml {
            // Repeated copies are separate documents.
            unit.insert_str(0, "---\n");
        }
        let copies = MB / unit.len() + 1;
        let source = unit.repeat(copies);
        let started = Instant::now();
        let _ = Snapshot::parse(language, String::new(), 0).expect("load");
        let load = started.elapsed();
        let parse = time(5, || {
            Snapshot::parse(language, source.clone(), 0).expect("parse")
        });
        let snapshot = Snapshot::parse(language, source.clone(), 0).expect("parse");
        let mid = (source.len() / 2..)
            .find(|i| source.is_char_boundary(*i))
            .unwrap_or(0);
        let range = viewport(&source, mid, 60);
        let highlight = time(31, || snapshot.highlights(range.clone()));
        println!(
            "| {language} | {} | {} | {} | {} | {} |",
            source.len(),
            snapshot.has_errors(),
            ms(parse),
            us(highlight),
            ms(load)
        );
    }
}

fn main() {
    let only = std::env::args().nth(1).filter(|a| !a.starts_with('-'));
    match only.as_deref() {
        Some("rust") => rust_1mb(),
        Some("cancel") => cancellation(),
        Some("languages") => per_language(),
        _ => {
            rust_1mb();
            cancellation();
            per_language();
        }
    }
}
