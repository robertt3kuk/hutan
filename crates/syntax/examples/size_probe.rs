//! Links every compiled grammar and exercises its parser and queries, so the release binary size
//! shows what the grammars cost (`docs/perf.md`): build it with different `--features` and compare
//! `target/release/examples/size_probe`.
#![allow(clippy::expect_used)] // a benchmark that cannot run should stop loudly

use hutan_syntax::{Language, Snapshot};

fn main() {
    let source = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x = 1\n".to_string());
    let mut work = 0;
    for language in Language::compiled() {
        let Ok(snapshot) = Snapshot::parse(language, source.clone(), 0) else {
            continue;
        };
        work += snapshot.highlights(0..source.len()).len() + snapshot.folds(0..source.len()).len();
    }
    println!("{work}");
}
