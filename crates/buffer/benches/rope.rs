//! Rope micro-benchmark behind the `crop` vs `ropey` decision (E-1, `docs/perf.md` § Editor).
//!
//! Run with `cargo bench -p hutan-buffer --bench rope` (release profile, no harness crate: plain
//! `Instant`, median of several runs). Both ropes are driven through the API the editor needs:
//! byte offsets in, byte offsets out. `crop` is byte-native; `ropey` 1.x indexes by `char`, so its
//! rows include the `byte_to_char` / `char_to_byte` conversions every call from tree-sitter, LSP or
//! the buffer API would pay. `ropey` 2.0 is still a beta (`2.0.0-beta.1`), so 1.6.1 is the stable
//! comparison.

use std::hint::black_box;
use std::time::{Duration, Instant};

/// Deterministic xorshift64* so both ropes see the same positions.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Source-like text of about `bytes` bytes: ~40-byte lines, every 7th with multi-byte characters
/// (2-, 3- and 4-byte UTF-8, so UTF-16 surrogate pairs occur).
fn corpus(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 128);
    let mut i = 0usize;
    while s.len() < bytes {
        if i % 7 == 3 {
            s.push_str(&format!("    // héllo 日本語 🦀 line {i}\n"));
        } else {
            s.push_str(&format!("    let value_{i} = compute(&state, {i})?;\n"));
        }
        i += 1;
    }
    s
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn time_runs<F: FnMut() -> Duration>(runs: usize, mut f: F) -> Duration {
    median((0..runs).map(|_| f()).collect())
}

fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1e3)
}

fn ns_per(d: Duration, ops: usize) -> String {
    format!("{:.0} ns/op", d.as_secs_f64() * 1e9 / ops as f64)
}

fn row(scenario: &str, crop: String, ropey: String) {
    println!("| {scenario} | {crop} | {ropey} |");
}

const EDITS: usize = 20_000;
const LOOKUPS: usize = 200_000;

fn main() {
    println!("| scenario | crop 0.4.3 | ropey 1.6.1 |");
    println!("|---|---|---|");
    for (label, size) in [("1 MB", 1 << 20), ("10 MB", 10 << 20)] {
        let text = corpus(size);
        let runs = if size > (1 << 20) { 5 } else { 9 };

        // Open: build the rope from a `&str` already in memory.
        let c = time_runs(runs, || {
            let t = Instant::now();
            black_box(crop::Rope::from(black_box(text.as_str())));
            t.elapsed()
        });
        let r = time_runs(runs, || {
            let t = Instant::now();
            black_box(ropey::Rope::from_str(black_box(text.as_str())));
            t.elapsed()
        });
        row(&format!("open {label}"), ms(c), ms(r));

        let crope = crop::Rope::from(text.as_str());
        let rrope = ropey::Rope::from_str(&text);
        let lines = crope.line_len();
        assert_eq!(lines, rrope.len_lines() - 1);

        // Snapshot: what a background parse or LSP sync takes.
        let c = time_runs(runs, || {
            let t = Instant::now();
            for _ in 0..1000 {
                black_box(crope.clone());
            }
            t.elapsed()
        });
        let r = time_runs(runs, || {
            let t = Instant::now();
            for _ in 0..1000 {
                black_box(rrope.clone());
            }
            t.elapsed()
        });
        row(
            &format!("snapshot (clone) {label}"),
            ns_per(c, 1000),
            ns_per(r, 1000),
        );

        // Random insert at a line start (bytes in): 20 000 edits on a fresh copy.
        let c = time_runs(runs, || {
            let mut rope = crope.clone();
            let mut rng = Rng(7);
            let t = Instant::now();
            for _ in 0..EDITS {
                let l = rng.below(rope.line_len());
                let b = rope.byte_of_line(l);
                rope.insert(b, "word ");
            }
            let d = t.elapsed();
            black_box(rope);
            d
        });
        let r = time_runs(runs, || {
            let mut rope = rrope.clone();
            let mut rng = Rng(7);
            let t = Instant::now();
            for _ in 0..EDITS {
                let l = rng.below(rope.len_lines() - 1);
                let b = rope.line_to_byte(l);
                let ci = rope.byte_to_char(b);
                rope.insert(ci, "word ");
            }
            let d = t.elapsed();
            black_box(rope);
            d
        });
        row(
            &format!("random insert {label}"),
            ns_per(c, EDITS),
            ns_per(r, EDITS),
        );

        // Random delete of up to 8 characters of a line, never past its end.
        let c = time_runs(runs, || {
            let mut rope = crope.clone();
            let mut rng = Rng(11);
            let t = Instant::now();
            for _ in 0..EDITS {
                let l = rng.below(rope.line_len());
                let b = rope.byte_of_line(l);
                let n: usize = rope
                    .line(l)
                    .chars()
                    .take(8)
                    .take_while(|c| *c != '\n')
                    .map(char::len_utf8)
                    .sum();
                rope.delete(b..b + n);
            }
            let d = t.elapsed();
            black_box(rope);
            d
        });
        let r = time_runs(runs, || {
            let mut rope = rrope.clone();
            let mut rng = Rng(11);
            let t = Instant::now();
            for _ in 0..EDITS {
                let l = rng.below(rope.len_lines() - 1);
                let b = rope.line_to_byte(l);
                let ci = rope.byte_to_char(b);
                let n = rope
                    .line(l)
                    .chars()
                    .take(8)
                    .take_while(|c| *c != '\n')
                    .count();
                rope.remove(ci..ci + n);
            }
            let d = t.elapsed();
            black_box(rope);
            d
        });
        row(
            &format!("random delete {label}"),
            ns_per(c, EDITS),
            ns_per(r, EDITS),
        );

        // Typing: 20 000 single characters at a moving cursor in the middle.
        let c = time_runs(runs, || {
            let mut rope = crope.clone();
            let at0 = rope.byte_of_line(rope.line_of_byte(rope.byte_len() / 2));
            let t = Instant::now();
            for i in 0..EDITS {
                rope.insert(at0 + i, "x");
            }
            let d = t.elapsed();
            black_box(rope);
            d
        });
        let r = time_runs(runs, || {
            let mut rope = rrope.clone();
            let b = rope.line_to_byte(rope.byte_to_line(rope.len_bytes() / 2));
            let at0 = rope.byte_to_char(b);
            let t = Instant::now();
            for i in 0..EDITS {
                rope.insert_char(at0 + i, 'x');
            }
            let d = t.elapsed();
            black_box(rope);
            d
        });
        row(
            &format!("typing at a cursor {label}"),
            ns_per(c, EDITS),
            ns_per(r, EDITS),
        );

        // Shared random positions: a line start plus 4 (the indentation is ASCII, so a char boundary).
        let mut rng = Rng(3);
        let positions: Vec<usize> = (0..LOOKUPS)
            .map(|_| crope.byte_of_line(rng.below(lines)) + 4)
            .collect();
        let line_idx: Vec<usize> = (0..LOOKUPS).map(|_| rng.below(lines)).collect();
        let utf16: Vec<usize> = positions
            .iter()
            .map(|b| crope.utf16_code_unit_of_byte(*b))
            .collect();

        // line -> byte and byte -> line.
        let c = time_runs(runs, || {
            let t = Instant::now();
            for l in &line_idx {
                black_box(crope.byte_of_line(*l));
            }
            t.elapsed()
        });
        let r = time_runs(runs, || {
            let t = Instant::now();
            for l in &line_idx {
                black_box(rrope.line_to_byte(*l));
            }
            t.elapsed()
        });
        row(
            &format!("line → byte {label}"),
            ns_per(c, LOOKUPS),
            ns_per(r, LOOKUPS),
        );
        let c = time_runs(runs, || {
            let t = Instant::now();
            for b in &positions {
                black_box(crope.line_of_byte(*b));
            }
            t.elapsed()
        });
        let r = time_runs(runs, || {
            let t = Instant::now();
            for b in &positions {
                black_box(rrope.byte_to_line(*b));
            }
            t.elapsed()
        });
        row(
            &format!("byte → line {label}"),
            ns_per(c, LOOKUPS),
            ns_per(r, LOOKUPS),
        );

        // UTF-16 conversions.
        let c = time_runs(runs, || {
            let t = Instant::now();
            for b in &positions {
                black_box(crope.utf16_code_unit_of_byte(*b));
            }
            t.elapsed()
        });
        let r = time_runs(runs, || {
            let t = Instant::now();
            for b in &positions {
                black_box(rrope.char_to_utf16_cu(rrope.byte_to_char(*b)));
            }
            t.elapsed()
        });
        row(
            &format!("byte → UTF-16 {label}"),
            ns_per(c, LOOKUPS),
            ns_per(r, LOOKUPS),
        );
        let c = time_runs(runs, || {
            let t = Instant::now();
            for u in &utf16 {
                black_box(crope.byte_of_utf16_code_unit(*u));
            }
            t.elapsed()
        });
        let r = time_runs(runs, || {
            let t = Instant::now();
            for u in &utf16 {
                black_box(rrope.char_to_byte(rrope.utf16_cu_to_char(*u)));
            }
            t.elapsed()
        });
        row(
            &format!("UTF-16 → byte {label}"),
            ns_per(c, LOOKUPS),
            ns_per(r, LOOKUPS),
        );
    }
}
