//! `hutan-fake-lsp --script <file.json>`: a scripted stand-in for a language server, for the
//! tests of `hutan-lsp-client` (never shipped). It speaks LSP over stdio with blocking I/O and
//! keeps its own mirror of every open document, applying `didChange` with its own position
//! arithmetic (independent of the client's), so a test can check what the server ended up
//! with. Everything is optional in the script:
//!
//! ```json
//! {
//!   "capabilities": {"positionEncoding": "utf-8", "textDocumentSync": 2, "hoverProvider": true},
//!   "diagnostics": true,            // after didOpen/didChange: one error per "error" in the text
//!   "progress": 2,                  // after initialized: create a token, begin, N reports, end
//!   "hover": "echo",                // or any text; "echo" answers with the mirror as JSON
//!   "definition": {"uri": "...", "range": {...}},
//!   "hang": ["textDocument/hover"], // requests never answered
//!   "crash": {"on": "initialize", "code": 3, "times": 1, "counter": "/tmp/count"},
//!   "flood": {"on": "initialized", "count": 10000, "method": "window/logMessage"},
//!   "stall": true,                  // stop reading stdin after initialized
//!   "spawn_child": true,            // start a child process, log "child <pid>"
//!   "configuration": ["fake.section"], // ask workspace/configuration, log the answer
//!   "ignore_exit": true             // stay alive after exit (the client must kill)
//! }
//! ```
//!
//! `crash.on` is a method name or `"start"`; without `times` it crashes every time, with it
//! only that many times (counted in the `counter` file across restarts). `flood.method` is
//! `window/logMessage` or `textDocument/publishDiagnostics` (one file per message). It logs
//! (`window/logMessage`) `cancelled <id>` for every `$/cancelRequest`.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Script {
    capabilities: Option<Value>,
    diagnostics: bool,
    progress: Option<u32>,
    hover: Option<String>,
    definition: Option<Value>,
    hang: Vec<String>,
    crash: Option<Crash>,
    flood: Option<Flood>,
    stall: bool,
    spawn_child: bool,
    configuration: Vec<String>,
    ignore_exit: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Crash {
    on: String,
    #[serde(default = "crash_code")]
    code: i32,
    times: Option<u32>,
    counter: Option<PathBuf>,
}

fn crash_code() -> i32 {
    3
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Flood {
    on: String,
    count: u32,
    method: String,
}

#[derive(Debug, Default)]
struct Doc {
    text: String,
    version: i64,
    opens: u32,
    changes: u32,
    full_changes: u32,
}

type Out = Arc<Mutex<io::Stdout>>;

fn send(out: &Out, message: &Value) {
    let body = message.to_string();
    let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
    // A closed stdout means the client is gone; the next read ends the loop.
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

fn notify(out: &Out, method: &str, params: Value) {
    send(
        out,
        &json!({"jsonrpc": "2.0", "method": method, "params": params}),
    );
}

fn reply(out: &Out, id: &Value, result: Value) {
    send(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn log(out: &Out, message: &str) {
    notify(
        out,
        "window/logMessage",
        json!({"type": 3, "message": message}),
    );
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Byte offset of an LSP position, clamped (its own arithmetic, not the client's).
fn to_byte(text: &str, position: &Value, utf8: bool) -> usize {
    let line = position["line"].as_u64().unwrap_or(0) as usize;
    let character = position["character"].as_u64().unwrap_or(0) as usize;
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let mut units = 0;
    for (i, c) in text[start..end].char_indices() {
        if units >= character {
            return start + i;
        }
        units += if utf8 { c.len_utf8() } else { c.len_utf16() };
    }
    end
}

fn to_position(text: &str, byte: usize, utf8: bool) -> Value {
    let before = &text[..byte];
    let line = before.matches('\n').count();
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    let character: usize = if utf8 {
        byte - start
    } else {
        text[start..byte].chars().map(char::len_utf16).sum()
    };
    json!({"line": line, "character": character})
}

fn publish(out: &Out, uri: &str, doc: &Doc, utf8: bool) {
    let diagnostics: Vec<Value> = doc
        .text
        .match_indices("error")
        .map(|(at, word)| {
            json!({
                "range": {
                    "start": to_position(&doc.text, at, utf8),
                    "end": to_position(&doc.text, at + word.len(), utf8),
                },
                "severity": 1,
                "source": "fake",
                "message": "found error",
            })
        })
        .collect();
    notify(
        out,
        "textDocument/publishDiagnostics",
        json!({"uri": uri, "version": doc.version, "diagnostics": diagnostics}),
    );
}

fn crash_now(crash: &Crash) -> bool {
    let (Some(times), Some(counter)) = (crash.times, &crash.counter) else {
        return true;
    };
    let count: u32 = std::fs::read_to_string(counter)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if count >= times {
        return false;
    }
    let _ = std::fs::write(counter, (count + 1).to_string());
    true
}

fn maybe_crash(script: &Script, on: &str) {
    if let Some(crash) = &script.crash
        && crash.on == on
        && crash_now(crash)
    {
        eprintln!("fake-lsp: crashing on {on}");
        std::process::exit(crash.code);
    }
}

fn maybe_flood(script: &Script, on: &str, out: &Out) {
    let Some(flood) = &script.flood else {
        return;
    };
    if flood.on != on {
        return;
    }
    let out = out.clone();
    let (count, method) = (flood.count, flood.method.clone());
    std::thread::spawn(move || {
        for i in 0..count {
            if method == "textDocument/publishDiagnostics" {
                let range = json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}});
                notify(
                    &out,
                    &method,
                    json!({"uri": format!("file:///flood/{i}.rs"), "diagnostics": [{"range": range, "message": format!("flood {i}")}]}),
                );
            } else {
                notify(
                    &out,
                    &method,
                    json!({"type": 4, "message": format!("flood {i}")}),
                );
            }
        }
        log(&out, "flood done");
    });
}

fn after_initialized(script: &Script, out: &Out) {
    if let Some(steps) = script.progress {
        send(
            out,
            &json!({"jsonrpc": "2.0", "id": "progress-create", "method": "window/workDoneProgress/create", "params": {"token": "fake-index"}}),
        );
        notify(
            out,
            "$/progress",
            json!({"token": "fake-index", "value": {"kind": "begin", "title": "Indexing", "percentage": 0}}),
        );
        for step in 1..=steps {
            notify(
                out,
                "$/progress",
                json!({"token": "fake-index", "value": {"kind": "report", "message": format!("{step}/{steps}"), "percentage": step * 100 / steps.max(1)}}),
            );
        }
        notify(
            out,
            "$/progress",
            json!({"token": "fake-index", "value": {"kind": "end"}}),
        );
    }
    if !script.configuration.is_empty() {
        let items: Vec<Value> = script
            .configuration
            .iter()
            .map(|section| json!({"section": section}))
            .collect();
        send(
            out,
            &json!({"jsonrpc": "2.0", "id": "config", "method": "workspace/configuration", "params": {"items": items}}),
        );
    }
    if script.spawn_child {
        let child = std::env::current_exe().and_then(|exe| {
            Command::new(exe)
                .arg("--child")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        });
        match child {
            Ok(child) => log(out, &format!("child {}", child.id())),
            Err(error) => log(out, &format!("child failed: {error}")),
        }
    }
}

fn hover(script: &Script, docs: &HashMap<String, Doc>, params: &Value) -> Value {
    let uri = params["textDocument"]["uri"].as_str().unwrap_or_default();
    match script.hover.as_deref() {
        Some("echo") => {
            let doc = docs.get(uri);
            let state = json!({
                "text": doc.map(|d| d.text.clone()),
                "version": doc.map(|d| d.version),
                "opens": doc.map_or(0, |d| d.opens),
                "changes": doc.map_or(0, |d| d.changes),
                "full_changes": doc.map_or(0, |d| d.full_changes),
            });
            json!({"contents": {"kind": "plaintext", "value": state.to_string()}})
        }
        Some(text) => json!({
            "contents": {"kind": "markdown", "value": text},
            "range": {"start": params["position"], "end": params["position"]},
        }),
        None => Value::Null,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--child") {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let script: Script = match args.as_slice() {
        [flag, path] if flag == "--script" => {
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("fake-lsp: cannot read {path}: {e}");
                std::process::exit(2)
            });
            serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("fake-lsp: bad script {path}: {e}");
                std::process::exit(2)
            })
        }
        _ => Script::default(),
    };
    let capabilities = script.capabilities.clone().unwrap_or_else(
        || json!({"textDocumentSync": 2, "hoverProvider": true, "definitionProvider": true}),
    );
    let utf8 = capabilities["positionEncoding"] == "utf-8";
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let mut input = BufReader::new(io::stdin());
    let mut docs: HashMap<String, Doc> = HashMap::new();
    maybe_crash(&script, "start");

    while let Some(message) = read_message(&mut input) {
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to one of our requests.
            if id == Some(json!("config")) {
                log(&out, &format!("configuration {}", message["result"]));
            }
            continue;
        };
        maybe_crash(&script, method);
        maybe_flood(&script, method, &out);
        if let Some(id) = &id
            && script.hang.iter().any(|m| m == method)
        {
            eprintln!("fake-lsp: not answering {method} {id}");
            continue;
        }
        let uri = params["textDocument"]["uri"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        match method {
            "initialize" => {
                if let Some(id) = &id {
                    reply(
                        &out,
                        id,
                        json!({"capabilities": capabilities, "serverInfo": {"name": "hutan-fake-lsp"}}),
                    );
                }
            }
            "initialized" => {
                after_initialized(&script, &out);
                if script.stall {
                    eprintln!("fake-lsp: no longer reading");
                    loop {
                        std::thread::sleep(Duration::from_secs(3600));
                    }
                }
            }
            "textDocument/didOpen" => {
                let item = &params["textDocument"];
                let doc = docs.entry(uri.clone()).or_default();
                doc.text = item["text"].as_str().unwrap_or_default().to_owned();
                doc.version = item["version"].as_i64().unwrap_or(0);
                doc.opens += 1;
                if script.diagnostics {
                    publish(&out, &uri, doc, utf8);
                }
            }
            "textDocument/didChange" => {
                let Some(doc) = docs.get_mut(&uri) else {
                    log(&out, &format!("change of unopened {uri}"));
                    continue;
                };
                for change in params["contentChanges"].as_array().into_iter().flatten() {
                    let text = change["text"].as_str().unwrap_or_default();
                    match change.get("range") {
                        Some(range) => {
                            let a = to_byte(&doc.text, &range["start"], utf8);
                            let b = to_byte(&doc.text, &range["end"], utf8);
                            doc.text.replace_range(a..b.max(a), text);
                        }
                        None => {
                            doc.text = text.to_owned();
                            doc.full_changes += 1;
                        }
                    }
                }
                doc.version = params["textDocument"]["version"].as_i64().unwrap_or(0);
                doc.changes += 1;
                if script.diagnostics {
                    publish(&out, &uri, doc, utf8);
                }
            }
            "textDocument/didSave" => log(&out, &format!("saved {uri}")),
            "textDocument/didClose" => {
                docs.remove(&uri);
                log(&out, &format!("closed {uri}"));
            }
            "textDocument/hover" => {
                if let Some(id) = &id {
                    reply(&out, id, hover(&script, &docs, &params));
                }
            }
            "textDocument/definition" => {
                if let Some(id) = &id {
                    let location = script.definition.clone().unwrap_or_else(|| {
                        json!({"uri": uri, "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}}})
                    });
                    reply(&out, id, location);
                }
            }
            "$/cancelRequest" => log(&out, &format!("cancelled {}", params["id"])),
            "shutdown" => {
                if let Some(id) = &id {
                    reply(&out, id, Value::Null);
                }
            }
            "exit" => {
                if !script.ignore_exit {
                    std::process::exit(0);
                }
                eprintln!("fake-lsp: ignoring exit");
            }
            _ => {
                if let Some(id) = &id {
                    send(
                        &out,
                        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("fake-lsp: no {method}")}}),
                    );
                }
            }
        }
    }
    if script.ignore_exit {
        // Stdin closed: stay until killed.
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
}
