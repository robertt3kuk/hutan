//! A fake `codex app-server`: replays a recorded transcript over stdio.
//!
//! The transcript is a sequence of `{"dir": "out" | "in", "msg": {...}}` lines (`out` = what the
//! client sent, `in` = what the server said), in the order they were observed. The replay walks
//! it: an `in` entry is written as soon as the entry before it is done; an `out` entry waits for
//! the client to send a message with the same method (or, for a response to a server request,
//! any response). A response entry is written with the id of the request it answers. After
//! `thread/resume` the recorded thread id is rewritten to the one the client asked for.
//!
//! A mismatch ends the process with exit code 3 and a message on stderr; the request that did
//! not match is answered with a JSON-RPC error first. `{"dir": "exit", "code": N}` ends the
//! process, and the end of the transcript keeps it alive until stdin closes.
//! `{"dir": "hold", "file": PATH}` pauses the replay until the file exists (at most
//! [`HOLD_LIMIT`]), so a test can stop a stream half way and release it when it is ready.
//!
//! When the transcript path is a directory, each spawn takes the next `*.jsonl` file in name
//! order (spawns are counted with marker files), so a test can script a session across daemon
//! restarts. Every message the client sends is appended to `received` when given.

use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Exit code of a replay that met an unexpected client message.
pub const EXIT_MISMATCH: i32 = 3;
/// How long a `hold` entry waits for its file.
pub const HOLD_LIMIT: Duration = Duration::from_secs(60);
/// Exit code of a replay that has no transcript left for this spawn.
pub const EXIT_NO_TRANSCRIPT: i32 = 4;

#[derive(Debug)]
enum Entry {
    Out(Value),
    In(Value),
    Exit(i32),
    Hold(PathBuf),
}

fn parse(transcript: &str) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for (index, line) in transcript.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let invalid = |what: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("transcript line {}: {what}", index + 1),
            )
        };
        let value: Value = serde_json::from_str(line).map_err(|e| invalid(&e.to_string()))?;
        let dir = value.get("dir").and_then(Value::as_str).unwrap_or("");
        let msg = value.get("msg").cloned();
        entries.push(match (dir, msg) {
            ("out", Some(msg)) => Entry::Out(msg),
            ("in", Some(msg)) => Entry::In(msg),
            ("hold", _) => Entry::Hold(PathBuf::from(
                value
                    .get("file")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("`hold` needs a `file`"))?,
            )),
            ("exit", _) => Entry::Exit(
                value
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok())
                    .unwrap_or(0),
            ),
            _ => return Err(invalid("expected `dir` out|in|exit|hold and a `msg`")),
        });
    }
    Ok(entries)
}

/// The transcript for this spawn: the file itself, or the next file of a directory.
fn pick(path: &Path) -> io::Result<Result<String, i32>> {
    if !path.is_dir() {
        return fs::read_to_string(path).map(Ok);
    }
    let mut files: Vec<PathBuf> = fs::read_dir(path)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "jsonl")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("received"))
        })
        .collect();
    files.sort();
    for spawn in 0.. {
        let marker = path.join(format!(".spawned-{spawn}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(_) => {
                return Ok(match files.get(spawn) {
                    Some(file) => Ok(fs::read_to_string(file)?),
                    None => {
                        eprintln!(
                            "fixture: no transcript for spawn {spawn} in {}",
                            path.display()
                        );
                        Err(EXIT_NO_TRANSCRIPT)
                    }
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    unreachable!("the spawn counter is unbounded")
}

fn method_of(message: &Value) -> Option<&str> {
    message.get("method").and_then(Value::as_str)
}

struct Server<R: BufRead, W: Write> {
    input: R,
    output: W,
    received: Option<fs::File>,
    spawn_label: String,
    /// Text replacements applied to every line written (recorded id → actual id).
    rewrites: Vec<(String, String)>,
    last_request: Option<(Value, Option<String>, Value)>,
}

impl<R: BufRead, W: Write> Server<R, W> {
    fn emit(&mut self, message: &Value) -> io::Result<()> {
        let mut line = message.to_string();
        for (from, to) in &self.rewrites {
            line = line.replace(from, to);
        }
        writeln!(self.output, "{line}")?;
        self.output.flush()
    }

    fn next_client_message(&mut self) -> io::Result<Option<Value>> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.input.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(line.trim()).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("client sent non-JSON: {e}"),
                )
            })?;
            if let Some(file) = &mut self.received {
                // One `write` per line: `writeln!` on a bare `File` is one syscall per piece,
                // and a test polling the log would read a line without its end.
                let entry = format!("{}\n", json!({"spawn": self.spawn_label, "msg": message}));
                file.write_all(entry.as_bytes())?;
            }
            return Ok(Some(message));
        }
    }

    /// Runs the transcript; `Ok(None)` when it ended normally, `Ok(Some(code))` to exit early.
    fn run(&mut self, entries: &[Entry]) -> io::Result<Option<i32>> {
        for entry in entries {
            match entry {
                Entry::Exit(code) => return Ok(Some(*code)),
                Entry::Hold(file) => {
                    let start = Instant::now();
                    while !file.exists() {
                        if start.elapsed() > HOLD_LIMIT {
                            eprintln!("fixture: {} never appeared", file.display());
                            return Ok(Some(EXIT_MISMATCH));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
                Entry::In(message) => {
                    let mut message = message.clone();
                    let is_response = method_of(&message).is_none() && message.get("id").is_some();
                    if is_response {
                        self.answer(&mut message);
                    }
                    self.emit(&message)?;
                }
                Entry::Out(expected) => {
                    let Some(got) = self.next_client_message()? else {
                        // The client left early; there is nobody to talk to.
                        return Ok(Some(0));
                    };
                    match (method_of(expected), method_of(&got)) {
                        (Some(want), Some(have)) if want == have => {
                            if let Some(id) = got.get("id") {
                                self.last_request = Some((
                                    id.clone(),
                                    Some(want.to_owned()),
                                    got.get("params").cloned().unwrap_or(Value::Null),
                                ));
                            }
                        }
                        (None, None) => {}
                        (want, have) => {
                            eprintln!(
                                "fixture: expected {:?}, the client sent {:?}: {got}",
                                want.unwrap_or("a response"),
                                have.unwrap_or("a response")
                            );
                            if let (Some(id), Some(_)) = (got.get("id"), have) {
                                self.emit(&json!({
                                    "id": id,
                                    "error": {"code": -32000, "message": "fixture: unexpected request"}
                                }))?;
                            }
                            return Ok(Some(EXIT_MISMATCH));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Gives a recorded response the id of the request it answers, and follows a resumed
    /// thread's id.
    fn answer(&mut self, response: &mut Value) {
        let Some((id, method, params)) = self.last_request.take() else {
            return;
        };
        response["id"] = id;
        if method.as_deref() == Some("thread/resume")
            && let (Some(recorded), Some(asked)) = (
                response
                    .pointer("/result/thread/id")
                    .and_then(Value::as_str),
                params.get("threadId").and_then(Value::as_str),
            )
            && recorded != asked
        {
            self.rewrites.push((recorded.to_owned(), asked.to_owned()));
        }
    }
}

/// Serves `transcript` on stdin/stdout. Returns the exit code.
pub fn run(transcript: &Path, received: Option<&Path>) -> io::Result<i32> {
    let text = match pick(transcript)? {
        Ok(text) => text,
        Err(code) => return Ok(code),
    };
    let entries = parse(&text)?;
    let received = match received {
        Some(path) => Some(OpenOptions::new().create(true).append(true).open(path)?),
        None => None,
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut server = Server {
        input: stdin.lock(),
        output: stdout.lock(),
        received,
        spawn_label: std::process::id().to_string(),
        rewrites: Vec::new(),
        last_request: None,
    };
    if let Some(code) = server.run(&entries)? {
        return Ok(code);
    }
    // Transcript done: like the real server, stay until the client closes stdin, still
    // recording what it says.
    while server.next_client_message()?.is_some() {}
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn serve(transcript: &str, client: &str) -> (Option<i32>, String) {
        let entries = parse(transcript).unwrap();
        let mut output = Vec::new();
        let mut server = Server {
            input: Cursor::new(client.as_bytes().to_vec()),
            output: &mut output,
            received: None,
            spawn_label: "test".to_owned(),
            rewrites: Vec::new(),
            last_request: None,
        };
        let code = server.run(&entries).unwrap();
        (code, String::from_utf8(output).unwrap())
    }

    const TRANSCRIPT: &str = r#"
{"dir":"out","msg":{"method":"initialize","id":1,"params":{}}}
{"dir":"in","msg":{"id":1,"result":{"ok":true}}}
{"dir":"in","msg":{"method":"hello","params":{}}}
{"dir":"out","msg":{"method":"thread/resume","id":2,"params":{"threadId":"recorded"}}}
{"dir":"in","msg":{"id":2,"result":{"thread":{"id":"recorded"}}}}
{"dir":"in","msg":{"method":"turn/started","params":{"threadId":"recorded"}}}
"#;

    #[test]
    fn responses_take_the_id_of_the_request_they_answer() {
        let client = "{\"method\":\"initialize\",\"id\":41,\"params\":{}}\n\
                      {\"method\":\"thread/resume\",\"id\":42,\"params\":{\"threadId\":\"actual\"}}\n";
        let (code, output) = serve(TRANSCRIPT, client);
        assert_eq!(code, None);
        let lines: Vec<Value> = output
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["id"], 41);
        assert_eq!(lines[1]["method"], "hello");
        assert_eq!(lines[2]["id"], 42);
        assert_eq!(lines[2]["result"]["thread"]["id"], "actual");
        assert_eq!(lines[3]["params"]["threadId"], "actual");
    }

    #[test]
    fn a_wrong_method_is_answered_with_an_error_and_ends_the_replay() {
        let (code, output) = serve(TRANSCRIPT, "{\"method\":\"model/list\",\"id\":7}\n");
        assert_eq!(code, Some(EXIT_MISMATCH));
        assert!(
            output.contains("\"id\":7") && output.contains("unexpected request"),
            "{output}"
        );
    }

    #[test]
    fn an_exit_entry_ends_the_replay_with_its_code() {
        let (code, _) = serve(r#"{"dir":"exit","code":9}"#, "");
        assert_eq!(code, Some(9));
    }

    #[test]
    fn a_hold_entry_waits_for_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let gate = dir.path().join("go");
        let transcript = format!(
            "{}\n{}\n",
            json!({"dir": "in", "msg": {"method": "before"}}),
            json!({"dir": "hold", "file": gate}),
        ) + &json!({"dir": "in", "msg": {"method": "after"}}).to_string();
        let opener = {
            let gate = gate.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                fs::write(gate, "").unwrap();
            })
        };
        let start = Instant::now();
        let (code, output) = serve(&transcript, "");
        opener.join().unwrap();
        assert_eq!(code, None);
        assert!(start.elapsed() >= Duration::from_millis(100), "it waited");
        assert_eq!(output.lines().count(), 2);
    }

    #[test]
    fn malformed_transcripts_name_the_line() {
        let error = parse("{\"dir\":\"sideways\",\"msg\":{}}").unwrap_err();
        assert!(error.to_string().contains("line 1"), "{error}");
    }
}
