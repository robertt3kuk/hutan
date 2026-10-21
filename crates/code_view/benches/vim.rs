//! B15 (`docs/spikes/2026-10-21-editor.md` § 4.8): a non-search Vim Normal command on a 1 MB
//! buffer, key → applied, through the rope-native host (`BufferHost`) the code view uses.
//!
//! Run with `cargo bench -p hutan-code-view --bench vim` (release profile, plain `Instant`).
//! Each sample lends a 1 MB `hutan_buffer::Buffer` to a host with the caret in the middle of the
//! file, runs one command's keys and takes the result back, as `CodeView::vim_key` does per key
//! (the GPUI frame is B12's).

use std::time::{Duration, Instant};

use hutan_buffer::Buffer;
use hutan_code_view::BufferHost;
use hutan_vim::{Key, Options, Vim, VimHost as _};

fn corpus(bytes: usize) -> String {
    let mut s = String::with_capacity(bytes + 128);
    let mut i = 0usize;
    while s.len() < bytes {
        s.push_str(&format!("    let value_{i} = compute(&state, {i})?;\n"));
        i += 1;
    }
    s
}

fn percentile(mut v: Vec<Duration>, p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn main() {
    let text = corpus(1_000_000);
    let options = Options {
        insert_passthrough: false,
        ..Options::default()
    };
    let commands: [(&str, &[Key]); 6] = [
        ("dd", &[Key::Char('d'), Key::Char('d')]),
        ("w", &[Key::Char('w')]),
        (
            "ciw + text + Esc",
            &[
                Key::Char('c'),
                Key::Char('i'),
                Key::Char('w'),
                Key::Char('x'),
                Key::Escape,
            ],
        ),
        (
            "yy then p",
            &[Key::Char('y'), Key::Char('y'), Key::Char('p')],
        ),
        ("G", &[Key::Char('G')]),
        ("u (after x)", &[Key::Char('x'), Key::Char('u')]),
    ];
    println!("| command (1 MB buffer) | p50 | p95 | max |");
    println!("|---|---|---|---|");
    for (name, keys) in commands {
        let mut samples = Vec::new();
        let mut buffer = Buffer::from_content(&text);
        for round in 0..200 {
            let middle = buffer.text().len() / 2 + round * 41;
            let mut vim: Vim<BufferHost> = Vim::new();
            let mut host = BufferHost::new(std::mem::take(&mut buffer), middle, true, false);
            vim.key(&mut host, Key::Escape, options, &mut ());
            let at = host.cursor();
            host.set_cursor(at);
            let started = Instant::now();
            for key in keys {
                vim.key(&mut host, *key, options, &mut ());
            }
            samples.push(started.elapsed());
            buffer = host.finish().0;
        }
        let ms = |d: Duration| format!("{:.3} ms", d.as_secs_f64() * 1e3);
        println!(
            "| `{name}` | {} | {} | {} |",
            ms(percentile(samples.clone(), 0.5)),
            ms(percentile(samples.clone(), 0.95)),
            ms(percentile(samples, 1.0)),
        );
    }
}
