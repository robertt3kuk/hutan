//! Differential fixtures (`tests/fixtures/*.json`, recorded from a real Vim by
//! `examples/record.rs`): every case replays on the reference host and has to end with Vim's
//! text, caret and registers. Recording is not part of the gate; replaying is.

use std::path::Path;

use hutan_vim::{Key, Mode, Options, RegisterKind, Vim, VimHost};
use serde_json::Value;

use crate::common::{Board, StringHost, parse_keys};

/// The Vim the fixtures are pinned to (the header of every file).
const PINNED: &str = "VIM - Vi IMproved 9.1";
/// The registers a case compares (the recorder stores the non-empty ones).
const REGISTERS: &str = "\"0123456789abcdefghijklmnopqrstuvwxyz-./:";
/// How many cases the fixtures must hold at least (the slice's acceptance).
const AT_LEAST: usize = 300;

fn lines_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("lines")
        .iter()
        .map(|l| l.as_str().expect("a line").to_owned())
        .collect()
}

/// Our register in Vim's `getreg()` / `getregtype()` form.
fn vim_form(vim: &Vim<StringHost>, name: char) -> Option<(String, String)> {
    let reg = vim.register_of(name)?;
    let (text, kind) = match reg.kind {
        RegisterKind::Char => (reg.text, "v".to_owned()),
        RegisterKind::Line => (format!("{}\n", reg.text), "V".to_owned()),
        RegisterKind::Block { width } => (reg.text, format!("\u{16}{width}")),
    };
    (!text.is_empty()).then_some((text, kind))
}

/// Replays one case; the differences from Vim, if any.
fn replay(case: &Value) -> Result<(), String> {
    let lines = lines_of(&case["lines"]);
    let text = lines.join("\n");
    let cursor = case["cursor"].as_array().expect("cursor");
    let (line, col) = (
        cursor[0].as_u64().expect("line") as usize,
        cursor[1].as_u64().expect("col") as usize,
    );
    let opts = Options {
        single_line: false,
        insert_passthrough: false,
        use_system_clipboard: false,
        command_line: true,
    };
    let mut clip = Board::default();
    let mut host = StringHost::with(&text, 0);
    let mut vim: Vim<StringHost> = Vim::new();
    vim.key(&mut host, Key::Escape, opts, &mut clip);
    if let Some(set) = case.get("set").and_then(Value::as_str) {
        // Like the recorder's script line: not typed, so `":` stays empty.
        let command = hutan_vim::ex::parse(&format!("set {set}"))
            .expect("a :set the fixture uses")
            .expect("a command");
        vim.run(&mut host, command, opts, &mut clip);
    }
    // Vim's `cursor()` clamps to the line.
    let ls: usize = lines[..line - 1].iter().map(|l| l.len() + 1).sum();
    let len = lines[line - 1].len();
    let mut at = ls + (col - 1).min(len.saturating_sub(1));
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    host = StringHost::with(&text, at);
    let keys = case["keys"].as_str().expect("keys");
    for key in parse_keys(keys) {
        vim.key(&mut host, key, opts, &mut clip);
    }
    // `feedkeys(…, 'x')` ends Insert mode as Escape does.
    if vim.mode() == Mode::Insert {
        vim.key(&mut host, Key::Escape, opts, &mut clip);
    }
    let expect = &case["expect"];
    let mut diffs = Vec::new();
    let want_lines = lines_of(&expect["lines"]);
    let got_lines: Vec<String> = host.text().split('\n').map(str::to_owned).collect();
    if want_lines != got_lines {
        diffs.push(format!("text {got_lines:?}, Vim {want_lines:?}"));
    }
    let want = expect["cursor"].as_array().expect("expect.cursor");
    let at = host.cursor();
    let got_line = host.text()[..at].matches('\n').count() + 1;
    let got_col = at - host.text()[..at].rfind('\n').map_or(0, |i| i + 1) + 1;
    let want_line = want[0].as_u64().unwrap_or(0) as usize;
    let want_col = want[1].as_u64().unwrap_or(0) as usize;
    if (got_line, got_col) != (want_line, want_col) {
        diffs.push(format!(
            "caret {got_line}:{got_col}, Vim {want_line}:{want_col}"
        ));
    }
    if vim.mode() != Mode::Normal {
        diffs.push(format!("ends in {:?}", vim.mode()));
    }
    let registers = expect["registers"].as_object().expect("expect.registers");
    for name in REGISTERS.chars() {
        let want = registers.get(&name.to_string()).map(|v| {
            let pair = v.as_array().expect("a register");
            (
                pair[0].as_str().unwrap_or_default().to_owned(),
                pair[1].as_str().unwrap_or_default().to_owned(),
            )
        });
        // Vim's internal key codes (`<80>kb` for a Backspace typed in `".`) are not UTF-8, so
        // the recorder cannot store them: such a register is not compared.
        if want
            .as_ref()
            .is_some_and(|(text, _)| text.contains('\u{FFFD}'))
        {
            continue;
        }
        let got = vim_form(&vim, name);
        if got != want {
            diffs.push(format!("register {name}: {got:?}, Vim {want:?}"));
        }
    }
    if diffs.is_empty() {
        Ok(())
    } else {
        Err(diffs.join("; "))
    }
}

#[test]
fn every_recorded_case_replays_like_vim() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("tests/fixtures")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let mut total = 0;
    let mut failures = Vec::new();
    for file in &files {
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(file).expect("read"))
            .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let stem = file.file_stem().unwrap_or_default().to_string_lossy();
        let vim = doc["vim"].as_str().unwrap_or_default();
        assert!(
            vim.starts_with(PINNED),
            "{stem}: recorded with `{vim}`, not {PINNED}"
        );
        for case in doc["cases"].as_array().expect("cases") {
            total += 1;
            let name = case["name"].as_str().unwrap_or("?");
            assert!(
                case.get("expect").is_some(),
                "{stem}/{name}: not recorded yet"
            );
            if let Err(diff) = replay(case) {
                failures.push(format!(
                    "{stem}/{name} ({:?}): {diff}",
                    case["keys"].as_str().unwrap_or_default()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} cases differ from Vim:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        total >= AT_LEAST,
        "{total} recorded cases, the slice asks for {AT_LEAST}"
    );
}
