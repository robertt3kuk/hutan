#![allow(clippy::unwrap_used)] // a bench over names it makes itself
//! B-EX (EX-1): the Files panel's tree model at 100 000 open files, no window and no daemon.
//!
//! `cargo run --release -p hutan-desktop --example files_tree_bench` (`just bench --only BEX`
//! runs it and holds the numbers to the budgets of `docs/perf.md`). It builds the listings of a
//! generated project — 100 packages × 10 modules × 100 files, 101 100 rows once everything is
//! open — and measures what the panel does with them on the UI thread:
//!
//! - `splice`: a listing arrives for one open folder (a file was added or removed): the rows of
//!   that folder are replaced, the other 100 000 stay;
//! - `unchanged`: the same listing arrives again (`fs.generation`): nothing is rebuilt;
//! - `full`: every row is built again (the root listed with a change, a filter toggled): what
//!   every listing cost before the splice;
//! - `expand`: a folder of 1 000 files is opened in the full tree;
//! - `rows_mb`: what the 101 100 rows and their listings hold.
//!
//! One JSON object on the last line of stdout, in milliseconds.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use hutan_desktop::workbench::panels::{FileTree, FileTreeOptions};
use hutan_protocol::fs::{FsEntry, FsKind, FsListing};

const PACKAGES: usize = 100;
const MODULES: usize = 10;
const FILES: usize = 100;
const SPLICES: usize = 300;
const FULLS: usize = 15;

fn entry(dir: &Path, name: String, kind: FsKind) -> FsEntry {
    FsEntry {
        path: dir.join(&name),
        name,
        kind,
        size: 100,
        mtime_ns: 1,
        ino: 0,
        ignored: false,
    }
}

fn listing(dir: &Path, entries: Vec<FsEntry>) -> FsListing {
    FsListing {
        dir: dir.to_owned(),
        entries,
        truncated: false,
    }
}

fn files_of(dir: &Path, count: usize) -> Vec<FsEntry> {
    (0..count)
        .map(|n| entry(dir, format!("file_{n:04}.rs"), FsKind::File))
        .collect()
}

fn rss_mb() -> f64 {
    Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

fn percentile(values: &mut [f64], p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    let rank = ((values.len() as f64 - 1.0) * p / 100.0).round() as usize;
    values[rank.min(values.len() - 1)]
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let root = PathBuf::from("/bench/project");
    let before = rss_mb();
    let mut tree = FileTree::new(root.clone(), FileTreeOptions::default());
    let packages: Vec<PathBuf> = (0..PACKAGES)
        .map(|p| root.join(format!("pkg{p:03}")))
        .collect();
    let modules: Vec<PathBuf> = packages
        .iter()
        .flat_map(|pkg| (0..MODULES).map(move |m| pkg.join(format!("mod{m}"))))
        .collect();

    // Everything open: the root, then each package, then each module, as the daemon answers.
    let started = Instant::now();
    tree.set_listing(
        &root,
        listing(
            &root,
            packages
                .iter()
                .map(|p| {
                    entry(
                        &root,
                        p.file_name().unwrap().to_string_lossy().into_owned(),
                        FsKind::Dir,
                    )
                })
                .collect(),
        ),
    );
    for pkg in &packages {
        tree.expand(pkg);
        tree.set_listing(
            pkg,
            listing(
                pkg,
                (0..MODULES)
                    .map(|m| entry(pkg, format!("mod{m}"), FsKind::Dir))
                    .collect(),
            ),
        );
    }
    for module in &modules {
        tree.expand(module);
        tree.set_listing(module, listing(module, files_of(module, FILES)));
    }
    let open_all_ms = ms(started);
    let rows = tree.rows().len();
    let rows_mb = rss_mb() - before;

    // A file added to, then removed from, one open module: the module's rows are replaced.
    let mut splice = Vec::with_capacity(SPLICES);
    for n in 0..SPLICES {
        let module = &modules[(n * 37) % modules.len()];
        let mut entries = files_of(module, FILES);
        if n % 2 == 0 {
            entries.push(entry(module, "added.rs".to_owned(), FsKind::File));
        }
        let fresh = listing(module, entries);
        let at = Instant::now();
        tree.set_listing(module, fresh);
        splice.push(ms(at));
    }
    // Leave the tree as it was: every module with its hundred files.
    for module in &modules {
        tree.set_listing(module, listing(module, files_of(module, FILES)));
    }

    // The same listing again: size and time changed, the entries did not.
    let mut unchanged = Vec::with_capacity(SPLICES);
    for n in 0..SPLICES {
        let module = &modules[(n * 53) % modules.len()];
        let mut entries = files_of(module, FILES);
        for e in &mut entries {
            e.mtime_ns = 2 + n as i64;
        }
        let fresh = listing(module, entries);
        let at = Instant::now();
        tree.set_listing(module, fresh);
        unchanged.push(ms(at));
    }

    // Every row again: the root listed with a new entry, then a filter toggled and back.
    let mut full = Vec::with_capacity(FULLS);
    for n in 0..FULLS {
        let mut entries: Vec<FsEntry> = packages
            .iter()
            .map(|p| {
                entry(
                    &root,
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    FsKind::Dir,
                )
            })
            .collect();
        if n % 2 == 0 {
            entries.push(entry(&root, "README.md".to_owned(), FsKind::File));
        }
        let fresh = listing(&root, entries);
        let at = Instant::now();
        tree.set_listing(&root, fresh);
        full.push(ms(at));
    }
    let hide_dotfiles = FileTreeOptions {
        show_hidden: false,
        ..FileTreeOptions::default()
    };
    let at = Instant::now();
    tree.set_options(hide_dotfiles);
    full.push(ms(at));
    let at = Instant::now();
    tree.set_options(FileTreeOptions::default());
    full.push(ms(at));

    // Opening a folder of 1 000 files in the full tree: the row it is on, its rows spliced in.
    let flat = root.join("pkg050/flat");
    let mut with_flat: Vec<FsEntry> = (0..MODULES)
        .map(|m| entry(&packages[50], format!("mod{m}"), FsKind::Dir))
        .collect();
    with_flat.push(entry(&packages[50], "flat".to_owned(), FsKind::Dir));
    tree.set_listing(&packages[50], listing(&packages[50], with_flat));
    let at = Instant::now();
    tree.expand(&flat);
    tree.set_listing(&flat, listing(&flat, files_of(&flat, 1_000)));
    let expand_ms = ms(at);

    println!(
        "{}",
        serde_json::json!({
            "rows": rows,
            "open_all_ms": open_all_ms,
            "splice_p50_ms": percentile(&mut splice.clone(), 50.0),
            "splice_p95_ms": percentile(&mut splice, 95.0),
            "unchanged_p95_ms": percentile(&mut unchanged, 95.0),
            "full_p50_ms": percentile(&mut full.clone(), 50.0),
            "full_p95_ms": percentile(&mut full, 95.0),
            "expand_1000_ms": expand_ms,
            "rows_mb": rows_mb,
            "rows_after_expand": tree.rows().len(),
        })
    );
}
