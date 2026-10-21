//! Records the differential fixtures of `crates/vim/tests/fixtures/*.json` from a real Vim
//! (dev only; the gate only replays them, `tests/it/fixtures.rs`).
//!
//! ```text
//! cargo run -p hutan-vim --example record                       # every fixture file
//! cargo run -p hutan-vim --example record -- motions registers  # some of them
//! ```
//!
//! Each case of a fixture file gives the text (`lines`), the caret (`cursor`, Vim's 1-based line
//! and byte column), optional `:set` options (`set`) and the keys in Vim's notation (`ihi<Esc>`,
//! `<C-v>`, `<CR>`, `<lt>` for `<`). The recorder writes the text to a file, opens it in
//! `/usr/bin/vim -Nu NONE -i NONE -es` (no vimrc, no viminfo, silent Ex mode), puts the caret,
//! types the keys with `feedkeys(…, 'tx')` (as typed: each command is its own undo step and
//! macros record what is typed), and stores what Vim ends with in the case's `expect`: the lines,
//! the caret and every register that is not empty. The Vim it used is pinned in the file header
//! (`vim`); a different Vim is refused unless `--any-vim` is given.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

const VIM: &str = "/usr/bin/vim";
const PINNED: &str = "VIM - Vi IMproved 9.1";
/// The registers a case records (the empty ones are left out).
const REGISTERS: &str = "\"0123456789abcdefghijklmnopqrstuvwxyz-./:";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let any_vim = args.iter().any(|a| a == "--any-vim");
    args.retain(|a| a != "--any-vim");
    let version = vim_version();
    if !version.starts_with(PINNED) && !any_vim {
        eprintln!(
            "{VIM} is `{version}`, the fixtures are pinned to `{PINNED}` (--any-vim to override)"
        );
        std::process::exit(2);
    }
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter(|p| {
            args.is_empty()
                || args
                    .iter()
                    .any(|a| p.file_stem().is_some_and(|s| s.to_string_lossy() == *a))
        })
        .collect();
    files.sort();
    let work = std::env::temp_dir().join(format!("hutan-vim-record-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let mut total = 0;
    for file in files {
        let mut doc: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let cases = doc["cases"].as_array_mut().unwrap();
        for case in cases.iter_mut() {
            let expect = record(&work, case);
            case["expect"] = expect;
            total += 1;
        }
        doc["vim"] = json!(format!("{version} ({VIM} -Nu NONE -i NONE -es)"));
        std::fs::write(&file, render(&doc)).unwrap();
        println!(
            "{}: {} cases",
            file.display(),
            doc["cases"].as_array().unwrap().len()
        );
    }
    std::fs::remove_dir_all(&work).ok();
    println!("{total} cases recorded with {version}");
}

/// `VIM - Vi IMproved 9.1 (…) patches 1-1752`.
fn vim_version() -> String {
    let out = Command::new(VIM).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default();
    let name = first.split(" (").next().unwrap_or(first);
    let patches = lines
        .find_map(|l| l.strip_prefix("Included patches: "))
        .unwrap_or("none");
    format!("{name}, patches {patches}")
}

fn record(work: &Path, case: &Value) -> Value {
    let name = case["name"].as_str().unwrap();
    let lines: Vec<&str> = case["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    let cursor = case["cursor"].as_array().unwrap();
    let keys = case["keys"].as_str().unwrap();
    let input = work.join("in.txt");
    let out = work.join("out.json");
    std::fs::remove_file(&out).ok();
    let mut text = lines.join("\n");
    if !(lines.len() == 1 && lines[0].is_empty()) {
        text.push('\n');
    }
    std::fs::write(&input, text).unwrap();
    let mut script = String::new();
    if let Some(set) = case.get("set").and_then(Value::as_str) {
        writeln!(script, "set {set}").unwrap();
    }
    writeln!(script, "call cursor({}, {})", cursor[0], cursor[1]).unwrap();
    writeln!(script, "call feedkeys(\"{}\", 'tx')", vim_string(keys)).unwrap();
    writeln!(script, "let regs = {{}}").unwrap();
    writeln!(
        script,
        "for r in split('{}', '\\zs')",
        REGISTERS.replace('\'', "''")
    )
    .unwrap();
    writeln!(
        script,
        "  if getreg(r) != '' | let regs[r] = [getreg(r), getregtype(r)] | endif"
    )
    .unwrap();
    writeln!(script, "endfor").unwrap();
    writeln!(
        script,
        "call writefile([json_encode({{'lines': getline(1, '$'), 'cursor': [line('.'), col('.')], 'registers': regs}})], '{}')",
        out.display()
    )
    .unwrap();
    writeln!(script, "qa!").unwrap();
    let script_path = work.join("s.vim");
    std::fs::write(&script_path, script).unwrap();
    let status = Command::new(VIM)
        .args([
            "-Nu",
            "NONE",
            "-i",
            "NONE",
            "-es",
            "--cmd",
            "set encoding=utf-8",
            "-S",
        ])
        .arg(&script_path)
        .arg(&input)
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::null())
        .status()
        .unwrap();
    let Ok(result) = std::fs::read_to_string(&out) else {
        panic!("case {name}: Vim wrote nothing ({status})");
    };
    let result: Value = serde_json::from_str(&result).unwrap();
    let mut registers = Map::new();
    if let Some(map) = result["registers"].as_object() {
        for (k, v) in map {
            registers.insert(k.clone(), v.clone());
        }
    }
    json!({
        "lines": result["lines"],
        "cursor": result["cursor"],
        "registers": Value::Object(registers),
    })
}

/// Keys in Vim's notation as the body of a Vim double-quoted string: `<Esc>` becomes `\<Esc>`.
fn vim_string(keys: &str) -> String {
    let mut out = String::new();
    let mut chars = keys.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let rest: String = chars.clone().collect();
                match rest.find('>') {
                    Some(end)
                        if end > 0
                            && rest[..end]
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '-') =>
                    {
                        out.push_str("\\<");
                        out.push_str(&rest[..=end]);
                        for _ in 0..=end {
                            chars.next();
                        }
                    }
                    _ => out.push('<'),
                }
            }
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// The file as it is checked in: the header, then one case per line, its fields in the order a
/// reader wants them (what it starts from, the keys, what Vim ended with).
fn render(doc: &Value) -> String {
    let mut out = String::from("{\n");
    for (key, value) in doc.as_object().unwrap() {
        if key == "cases" {
            continue;
        }
        writeln!(out, "  {}: {},", json!(key), value).unwrap();
    }
    out.push_str("  \"cases\": [\n");
    let cases = doc["cases"].as_array().unwrap();
    for (i, case) in cases.iter().enumerate() {
        let fields: Vec<String> = ["name", "lines", "cursor", "set", "keys", "expect"]
            .iter()
            .filter_map(|k| case.get(*k).map(|v| format!("{}: {v}", json!(k))))
            .collect();
        let sep = if i + 1 < cases.len() { "," } else { "" };
        writeln!(out, "    {{{}}}{sep}", fields.join(", ")).unwrap();
    }
    out.push_str("  ]\n}\n");
    out
}
