//! The real Claude Code (`claude -p`) and the real Codex (`codex exec`) in a Hutan terminal, under
//! a test daemon, against a mock model API on the loopback (TEL-2, ADR 0026 decision 8): no login,
//! no network, no paid call. What they send as OpenTelemetry reaches the receiver through the shell
//! functions of the shell integration or through `hutan integration install … --telemetry`, and
//! becomes the insight of their terminal session; an OpenTelemetry setting that was there first is
//! reported and left alone.
//!
//! The two agents must be installed (`claude` and `codex` on `PATH`): a computer without them skips
//! these tests with a message. Each agent runs with a scratch `HOME`, `CLAUDE_CONFIG_DIR` and
//! `CODEX_HOME`, proxies that lead nowhere and no auto-update: nothing here reads or writes the
//! user's own `~/.claude` or `~/.codex`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

use crate::common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use common::{HUTAN, TestDaemon, command, finish};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{sleep, timeout};

/// A real agent run takes a few seconds; Codex starts slowly.
const RUN: Duration = Duration::from_secs(60);

// ---- the mock ------------------------------------------------------------------------------------

/// The Anthropic Messages API and the OpenAI Responses API of a model that answers a prompt with
/// one tool call and then an answer, with fixed usage numbers; and a collector that takes any
/// OpenTelemetry request and says 200 (the "user's own collector" of the conflict test). Every
/// request is recorded as `METHOD path`.
struct Mock {
    port: u16,
    stop: Arc<AtomicBool>,
    seen: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl Mock {
    fn start(work: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let (stop, seen, work) = (stop.clone(), seen.clone(), work.to_owned());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let (seen, work) = (seen.clone(), work.clone());
                    std::thread::spawn(move || serve(stream, &work, &seen));
                }
            })
        };
        Self {
            port,
            stop,
            seen,
            thread: Some(thread),
        }
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn count(&self, needle: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(mut stream: TcpStream, work: &Path, seen: &Mutex<Vec<String>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (
        request.next().unwrap_or_default().to_owned(),
        request.next().unwrap_or_default().to_owned(),
    );
    let path = target.split('?').next().unwrap_or_default().to_owned();
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buffer.len() < head_end + length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
    let body: Value =
        serde_json::from_slice(&buffer[head_end..head_end + length]).unwrap_or_default();
    seen.lock().unwrap().push(format!("{method} {path}"));
    match (method.as_str(), path.as_str()) {
        ("POST", p) if p.ends_with("/v1/messages") => anthropic(&mut stream, &body, work),
        ("POST", p) if p.ends_with("/responses") => responses(&mut stream, &body),
        ("POST", p) if p.ends_with("/count_tokens") => {
            reply(&mut stream, "application/json", br#"{"input_tokens":100}"#);
        }
        ("POST", p)
            if ["/v1/logs", "/v1/traces", "/v1/metrics"]
                .iter()
                .any(|s| p.ends_with(s)) =>
        {
            reply(&mut stream, "application/x-protobuf", b"");
        }
        ("GET", _) => reply(
            &mut stream,
            "application/json",
            br#"{"data":[],"object":"list"}"#,
        ),
        _ => reply(&mut stream, "application/json", b"{}"),
    }
}

fn reply(stream: &mut TcpStream, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn event_stream(stream: &mut TcpStream, events: &[(&str, Value)]) {
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for (name, data) in events {
        let _ = stream.write_all(format!("event: {name}\ndata: {data}\n\n").as_bytes());
    }
    let _ = stream.flush();
}

/// Anthropic: the first request of the main model that offers `Read` asks for it (1 200 input,
/// 3 000 cache written, 30 output); the next one answers (1 500 input, 3 000 cache read, 12 output).
fn anthropic(stream: &mut TcpStream, request: &Value, work: &Path) {
    let messages = request["messages"].as_array().cloned().unwrap_or_default();
    let has_result = messages.iter().any(|message| {
        message["content"]
            .as_array()
            .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_result"))
    });
    let model = request["model"].as_str().unwrap_or("claude-sonnet-5");
    let offers_read = request["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "Read"));
    let main = model.starts_with("claude-") && !model.contains("haiku");
    let asks_for_a_tool = !has_result && offers_read && main;
    let (usage, output, stop, start, delta) = if asks_for_a_tool {
        (
            json!({"input_tokens": 1200, "cache_creation_input_tokens": 3000, "cache_read_input_tokens": 0, "output_tokens": 1}),
            30,
            "tool_use",
            json!({"type": "tool_use", "id": "toolu_mock01", "name": "Read", "input": {}}),
            json!({"type": "input_json_delta", "partial_json": json!({"file_path": work.join("hello.txt")}).to_string()}),
        )
    } else {
        (
            json!({"input_tokens": 1500, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 3000, "output_tokens": 1}),
            12,
            "end_turn",
            json!({"type": "text", "text": ""}),
            json!({"type": "text_delta", "text": "Mock answer: hello from the local mock."}),
        )
    };
    if request["stream"] != true {
        let mut usage = usage;
        usage["output_tokens"] = json!(output);
        let body = json!({"id": "msg_mock", "type": "message", "role": "assistant", "model": model,
            "content": [{"type": "text", "text": "Mock title"}], "stop_reason": "end_turn", "usage": usage});
        return reply(stream, "application/json", body.to_string().as_bytes());
    }
    event_stream(
        stream,
        &[
            (
                "message_start",
                json!({"type": "message_start", "message": {"id": "msg_mock", "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "stop_sequence": null, "usage": usage}}),
            ),
            (
                "content_block_start",
                json!({"type": "content_block_start", "index": 0, "content_block": start}),
            ),
            (
                "content_block_delta",
                json!({"type": "content_block_delta", "index": 0, "delta": delta}),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            (
                "message_delta",
                json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": output}}),
            ),
            ("message_stop", json!({"type": "message_stop"})),
        ],
    );
}

/// OpenAI Responses: the first request that offers tools runs `echo hi` (2 000 input, 40 output),
/// the next answers (2 100 input of which 1 500 cached, 15 output of which 5 reasoning).
fn responses(stream: &mut TcpStream, request: &Value) {
    let input = request["input"].as_array().cloned().unwrap_or_default();
    let has_output = input.iter().any(|item| {
        matches!(
            item["type"].as_str(),
            Some("function_call_output" | "local_shell_call_output")
        )
    });
    let (item, usage) = if !has_output && request["tools"].as_array().is_some_and(|t| !t.is_empty())
    {
        (
            json!({"type": "function_call", "id": "fc_mock1", "call_id": "call_mock1", "name": "exec_command",
                "arguments": json!({"cmd": "echo hi", "yield_time_ms": 1000}).to_string()}),
            json!({"input_tokens": 2000, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 40, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2040}),
        )
    } else {
        (
            json!({"type": "message", "role": "assistant", "id": "msg_mock1",
                "content": [{"type": "output_text", "text": "Mock answer: hello from the local mock."}]}),
            json!({"input_tokens": 2100, "input_tokens_details": {"cached_tokens": 1500}, "output_tokens": 15, "output_tokens_details": {"reasoning_tokens": 5}, "total_tokens": 2115}),
        )
    };
    event_stream(
        stream,
        &[
            (
                "response.created",
                json!({"type": "response.created", "response": {"id": "resp_mock"}}),
            ),
            (
                "response.output_item.done",
                json!({"type": "response.output_item.done", "item": item}),
            ),
            (
                "response.completed",
                json!({"type": "response.completed", "response": {"id": "resp_mock", "usage": usage}}),
            ),
        ],
    );
}

// ---- a computer, a daemon and a terminal ---------------------------------------------------------

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// The agent's folders and the mock, in a temporary directory. The Claude folder has a space in
/// its path (the header helper's command must survive it).
struct Fixture {
    _root: TempDir,
    work: PathBuf,
    home: PathBuf,
    claude: PathBuf,
    codex: PathBuf,
    out: PathBuf,
    managed: PathBuf,
    mock: Mock,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().canonicalize().unwrap();
        let work = base.join("work");
        let home = base.join("home");
        let claude = base.join("claude home").join(".claude");
        let codex = base.join("codex home").join(".codex");
        let out = base.join("out");
        let managed = base.join("managed");
        for dir in [&work, &home, &claude, &codex, &out, &managed] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(work.join("hello.txt"), "hello from the fixture\n").unwrap();
        let mock = Mock::start(&work);
        std::fs::write(
            codex.join("config.toml"),
            format!(
                "model = \"gpt-mock\"\nmodel_provider = \"mock\"\n\n[model_providers.mock]\n\
                 name = \"mock\"\nbase_url = \"{}/v1\"\nenv_key = \"MOCK_API_KEY\"\n\
                 wire_api = \"responses\"\n",
                mock.base()
            ),
        )
        .unwrap();
        Self {
            _root: root,
            work,
            home,
            claude,
            codex,
            out,
            managed,
            mock,
        }
    }

    /// `hutan args…` as the user would run it outside the terminal: against the fixture's agent
    /// folders and the test daemon's home (the receiver's port is in its `daemon.toml`).
    async fn hutan(&self, t: &TestDaemon, args: &[&str]) -> common::Out {
        let mut command = command(Path::new(HUTAN), Some(&t.paths.socket), args);
        command
            .env("HUTAN_HOME", &t.paths.base)
            .env("HUTAN_MANAGED_CONFIG_ROOT", &self.managed)
            .env("HOME", &self.home)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("CODEX_HOME", &self.codex);
        for variable in [
            "CLAUDE_CODE_ENABLE_TELEMETRY",
            "OTEL_LOGS_EXPORTER",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
        ] {
            command.env_remove(variable);
        }
        finish(command).await
    }
}

/// A daemon whose terminals get the daemon's own environment (so `claude` and `codex` are on
/// their `PATH`), with a short idle close: the logs of an agent never say a turn ended.
async fn daemon(agents: &[&str]) -> TestDaemon {
    let t = TestDaemon::start_toml("[terminal]\nenv_inherit = \"daemon\"\n", |config| {
        // Longer than the pause of a real agent between its model call and the tool's end (Codex
        // starts a login shell for each command), short enough for a test.
        config.telemetry.idle_close = Duration::from_secs(4);
    })
    .await;
    for agent in agents {
        t.hutan(&["telemetry", "enable", agent]).await.ok();
    }
    t
}

/// An interactive bash with Hutan's shell integration, as the daemon starts one, in which the
/// agent has reported itself (what its hooks do): from then on the session has an insight.
/// Everything the agents could read of the developer's own environment is dropped first, and the
/// agents are pointed at the mock.
async fn terminal(t: &TestDaemon, f: &Fixture, agent: &str) -> String {
    let id = t.new_session(&[], &["/bin/bash", "--norc", "-i"]).await;
    // `hutan agent report` gives up after 200 ms and still exits 0 (an agent's hook must never
    // hold the agent up): a daemon busy starting a shell may miss one, so report until the
    // session has an insight (it has one once an agent is known in it).
    let shown = timeout(Duration::from_secs(30), async {
        let mut seq = 0u32;
        while t.hutan(&["chat", "insight", &id, "--json"]).await.code != 0 {
            seq += 1;
            let mut report = command(
                Path::new(HUTAN),
                Some(&t.paths.socket),
                &[
                    "agent",
                    "report",
                    "--source",
                    "test",
                    "--agent",
                    agent,
                    "--state",
                    "idle",
                    "--seq",
                    &seq.to_string(),
                ],
            );
            report.env("HUTAN_SESSION_ID", &id);
            finish(report).await.ok();
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        shown.is_ok(),
        "the agent report never showed up: {}",
        t.session(&id).await
    );
    let key = format!("sk-ant-{}", "api03-mockmockmockmockmockmock");
    for line in [
        "for v in $(compgen -e | grep -E '^(OTEL_|CLAUDE_|ANTHROPIC_|CODEX_|OPENAI_|HTTPS?_PROXY|NO_PROXY)'); do unset $v; done".to_owned(),
        format!(
            "export HOME='{}' CLAUDE_CONFIG_DIR='{}' CODEX_HOME='{}' ANTHROPIC_BASE_URL='{}' \
             ANTHROPIC_API_KEY='{key}' MOCK_API_KEY=mock-key DISABLE_TELEMETRY=1 \
             DISABLE_ERROR_REPORTING=1 DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
             HTTPS_PROXY=http://127.0.0.1:9 HTTP_PROXY=http://127.0.0.1:9 NO_PROXY=127.0.0.1,localhost",
            f.home.display(),
            f.claude.display(),
            f.codex.display(),
            f.mock.base(),
        ),
        format!("cd '{}'", f.work.display()),
    ] {
        t.hutan(&["send", &id, &line, "--enter"]).await.ok();
    }
    id
}

async fn type_line(t: &TestDaemon, id: &str, line: &str) {
    t.hutan(&["send", id, line, "--enter"]).await.ok();
}

const CLAUDE_PROMPT: &str = "Read hello.txt and tell me what it says";

/// `claude -p` with its output kept in `out/<name>.json|err` (`prefix` is `command ` to bypass the
/// function: a backslash would only bypass an alias).
fn claude_line(f: &Fixture, prefix: &str, name: &str) -> String {
    format!(
        "{prefix}claude -p '{CLAUDE_PROMPT}' --output-format json --allowedTools Read > '{0}/{name}.json' 2> '{0}/{name}.err'",
        f.out.display()
    )
}

fn codex_line(f: &Fixture, prefix: &str, name: &str) -> String {
    format!(
        "{prefix}codex exec --skip-git-repo-check 'Say hi' > '{0}/{name}.out' 2>&1",
        f.out.display()
    )
}

async fn insight(t: &TestDaemon, id: &str) -> Value {
    t.hutan(&["chat", "insight", id, "--json"])
        .await
        .ok()
        .json()
}

async fn telemetry_status(t: &TestDaemon) -> Value {
    t.hutan(&["telemetry", "status", "--json"])
        .await
        .ok()
        .json()
}

/// What a failed wait shows: the agent's own output and what the mock and the receiver saw.
async fn evidence(t: &TestDaemon, f: &Fixture) -> String {
    let seen = f.mock.seen.lock().unwrap().clone();
    let mut text = format!(
        "mock saw: {seen:?}\nreceiver: {}\n",
        telemetry_status(t).await
    );
    for entry in std::fs::read_dir(&f.out).unwrap().flatten() {
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        text.push_str(&format!(
            "--- {}\n{}\n",
            entry.file_name().to_string_lossy(),
            content.chars().take(1500).collect::<String>()
        ));
    }
    text
}

async fn wait_insight(
    t: &TestDaemon,
    f: &Fixture,
    id: &str,
    what: &str,
    pred: impl Fn(&Value) -> bool,
) -> Value {
    let found = timeout(RUN, async {
        loop {
            let now = insight(t, id).await;
            if pred(&now) {
                return now;
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    match found {
        Ok(now) => now,
        Err(_) => panic!(
            "the insight never showed {what}\ninsight: {}\n{}",
            insight(t, id).await,
            evidence(t, f).await
        ),
    }
}

fn source_of<'a>(insight: &'a Value, block: &str) -> Option<&'a str> {
    insight["sources"]
        .as_array()?
        .iter()
        .find(|source| source["block"] == block)
        .and_then(|source| source["source"].as_str())
}

/// What the mock Claude run must have become: tokens (uncached input, cache read and written,
/// output), the one `Read`, all from the receiver.
fn assert_claude(insight: &Value) {
    let totals = &insight["totals"];
    assert_eq!(totals["input"], 2_700, "{insight}");
    assert_eq!(totals["cache_read"], 3_000, "{insight}");
    assert_eq!(totals["cache_write"], 3_000, "{insight}");
    assert_eq!(totals["output"], 42, "{insight}");
    assert_eq!(insight["tools"]["calls"], 1, "{insight}");
    assert_eq!(insight["tools"]["by_name"][0]["name"], "Read", "{insight}");
    assert_eq!(source_of(insight, "turns"), Some("otlp"), "{insight}");
}

fn assert_codex(insight: &Value) {
    let totals = &insight["totals"];
    // Codex counts the cache inside the input: 2 000 + 2 100.
    assert_eq!(totals["input"], 4_100, "{insight}");
    assert_eq!(totals["cache_read"], 1_500, "{insight}");
    assert_eq!(totals["output"], 55, "{insight}");
    assert_eq!(insight["tools"]["calls"], 1, "{insight}");
    assert_eq!(
        insight["tools"]["by_name"][0]["name"], "exec_command",
        "{insight}"
    );
    assert_eq!(source_of(insight, "turns"), Some("otlp"), "{insight}");
}

/// The model the receiver says the session's agent used.
async fn model_of(t: &TestDaemon, id: &str) -> Option<String> {
    telemetry_status(t).await["sessions"]
        .as_array()?
        .iter()
        .find(|session| session["session_id"] == id)
        .and_then(|session| session["model"].as_str().map(str::to_owned))
}

/// The agent's own answer: the function around it did not get in its way.
fn answered(f: &Fixture, name: &str) -> bool {
    std::fs::read_to_string(f.out.join(format!("{name}.json")))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|answer| {
            answer["is_error"] == false
                && answer["result"]
                    .as_str()
                    .is_some_and(|result| result.contains("Mock answer"))
        })
}

// ---- Claude Code ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn claude_code_in_a_hutan_terminal_reports_through_the_shell_function() {
    if !on_path("claude") {
        return eprintln!("skipping: claude is not installed");
    }
    let f = Fixture::new();
    let t = daemon(&["claude"]).await;
    let id = terminal(&t, &f, "claude").await;
    type_line(&t, &id, &claude_line(&f, "", "claude")).await;
    let insight = wait_insight(&t, &f, &id, "Claude Code's tokens", |i| {
        i["totals"]["input"] == 2_700
    })
    .await;
    assert_claude(&insight);
    assert_eq!(model_of(&t, &id).await.as_deref(), Some("claude-opus-5-5"));
    assert!(answered(&f, "claude"), "{}", evidence(&t, &f).await);
    // Nothing of Hutan's was written to Claude's own configuration: the function is launch-scoped.
    assert!(!f.claude.join("settings.json").exists());
    assert!(!f.claude.join("hooks").exists());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_code_with_its_own_otel_settings_keeps_them_and_hutan_says_so() {
    if !on_path("claude") {
        return eprintln!("skipping: claude is not installed");
    }
    let f = Fixture::new();
    // The user's settings export to their own collector (here the mock).
    let settings = json!({"env": {
        "CLAUDE_CODE_ENABLE_TELEMETRY": "1",
        "OTEL_LOGS_EXPORTER": "otlp",
        "OTEL_EXPORTER_OTLP_PROTOCOL": "http/protobuf",
        "OTEL_EXPORTER_OTLP_ENDPOINT": f.mock.base(),
    }});
    let mut text = serde_json::to_string_pretty(&settings).unwrap();
    text.push('\n');
    std::fs::write(f.claude.join("settings.json"), &text).unwrap();
    let t = daemon(&["claude"]).await;
    let id = terminal(&t, &f, "claude").await;
    type_line(&t, &id, &claude_line(&f, "", "claude")).await;
    // Claude Code honoured its settings: the export went to the user's collector...
    let waited = timeout(RUN, async {
        while f.mock.count("POST /v1/logs") == 0 || !answered(&f, "claude") {
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{}", evidence(&t, &f).await);
    // ...and Hutan's receiver got nothing: the function added nothing.
    let status = telemetry_status(&t).await;
    assert_eq!(status["counters"]["accepted"], 0, "{status}");
    assert_eq!(status["counters"]["rejected"], 0, "{status}");
    assert!(insight(&t, &id).await["totals"].is_null());
    // The settings are untouched and `status` says what is in the way, without the values.
    assert_eq!(
        std::fs::read_to_string(f.claude.join("settings.json")).unwrap(),
        text
    );
    let said = f
        .hutan(&t, &["integration", "status", "claude"])
        .await
        .ok()
        .stdout;
    assert!(
        said.contains("telemetry conflict:") && said.contains("OTEL_EXPORTER_OTLP_ENDPOINT"),
        "{said}"
    );
    assert!(!said.contains(&f.mock.base()), "{said}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_code_started_outside_the_function_reports_through_the_installed_settings() {
    if !on_path("claude") {
        return eprintln!("skipping: claude is not installed");
    }
    let f = Fixture::new();
    let t = daemon(&["claude"]).await;
    let installed = f
        .hutan(&t, &["integration", "install", "claude", "--telemetry"])
        .await
        .ok();
    assert!(
        installed.stdout.contains("telemetry: installed"),
        "{}",
        installed.stdout
    );
    let id = terminal(&t, &f, "claude").await;
    // `command claude` is not the function: only the settings and the header helper are in play,
    // and the helper reads the session's token from the environment of this terminal.
    type_line(&t, &id, &claude_line(&f, "command ", "claude")).await;
    let insight = wait_insight(&t, &f, &id, "Claude Code's tokens", |i| {
        i["totals"]["input"] == 2_700
    })
    .await;
    assert_claude(&insight);
    assert!(answered(&f, "claude"), "{}", evidence(&t, &f).await);
    // An uninstall leaves the folder as it was.
    f.hutan(&t, &["integration", "uninstall", "claude"])
        .await
        .ok();
    assert!(
        !f.claude
            .join("hooks")
            .join("hutan-otel-headers.sh")
            .exists()
    );
    let left = std::fs::read_to_string(f.claude.join("settings.json")).unwrap_or_default();
    assert!(!left.contains("OTEL_"), "{left}");
    t.stop().await;
}

// ---- Codex ---------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn codex_in_a_hutan_terminal_reports_through_the_shell_function() {
    if !on_path("codex") {
        return eprintln!("skipping: codex is not installed");
    }
    let f = Fixture::new();
    let t = daemon(&["codex"]).await;
    let id = terminal(&t, &f, "codex").await;
    type_line(&t, &id, &codex_line(&f, "", "codex")).await;
    let insight = wait_insight(&t, &f, &id, "Codex's tokens", |i| {
        i["totals"]["input"] == 4_100
    })
    .await;
    assert_codex(&insight);
    assert_eq!(model_of(&t, &id).await.as_deref(), Some("gpt-mock"));
    // Codex's own configuration has no `[otel]` of Hutan's: the function put it on the command.
    let config = std::fs::read_to_string(f.codex.join("config.toml")).unwrap();
    assert!(!config.contains("otel"), "{config}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_with_the_installed_block_needs_the_function_for_its_token_and_says_so() {
    if !on_path("codex") {
        return eprintln!("skipping: codex is not installed");
    }
    let f = Fixture::new();
    let t = daemon(&["codex"]).await;
    let before = std::fs::read_to_string(f.codex.join("config.toml")).unwrap();
    f.hutan(&t, &["integration", "install", "codex", "--telemetry"])
        .await
        .ok();
    let id = terminal(&t, &f, "codex").await;
    // Through the function: the block carries the endpoint, the function the session's token.
    type_line(&t, &id, &codex_line(&f, "", "with-function")).await;
    let insight = wait_insight(&t, &f, &id, "Codex's tokens", |i| {
        i["totals"]["input"] == 4_100
    })
    .await;
    assert_codex(&insight);
    // Without the function nothing says which session this is: the receiver refuses, the agent
    // runs on and nothing is added.
    let accepted = telemetry_status(&t).await["counters"]["accepted"].clone();
    type_line(&t, &id, &codex_line(&f, "command ", "without")).await;
    let refused = timeout(RUN, async {
        loop {
            let status = telemetry_status(&t).await;
            if status["counters"]["rejected"].as_u64().unwrap_or(0) > 0 {
                return status;
            }
            sleep(Duration::from_millis(250)).await;
        }
    })
    .await;
    let refused = match refused {
        Ok(status) => status,
        Err(_) => panic!("no request was refused\n{}", evidence(&t, &f).await),
    };
    assert_eq!(refused["counters"]["accepted"], accepted, "{refused}");
    // An uninstall gives config.toml back byte for byte.
    f.hutan(&t, &["integration", "uninstall", "codex"])
        .await
        .ok();
    assert_eq!(
        std::fs::read_to_string(f.codex.join("config.toml")).unwrap(),
        before
    );
    t.stop().await;
}
