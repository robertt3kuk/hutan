//! The loopback OTLP receiver (TEL-1, protocol 0.74, capability `agent.telemetry`, ADR 0026), over
//! real sockets: an in-process daemon on a temporary `HUTAN_HOME`, sessions that print the
//! endpoint and token they were given, and requests written by hand so that every header, the
//! `Host`, the chunking and the size are the test's.
//!
//! The payloads are the spike's recordings (`docs/spikes/2026-11-03-agent-telemetry/fixtures/`),
//! sent as recorded (OTLP-JSON) and as the protobuf `hutan_telemetry::encode` builds from them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::{self, TestDaemon, WAIT, sh};

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use hutan_client::Client;
use hutan_core::SessionId;
use hutan_core::insight::{AgentInsight, InsightBlock, InsightSourceKind};
use hutan_protocol::messages::{AgentApplied, AgentReportRequest, AgentState, Request as Wire};
use hutan_protocol::telemetry::{ListenerState, TelemetryStatus};
use hutan_telemetry::{Signal, encode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout};

// What a test sends ----------------------------------------------------------------------------

fn recording(dir: &str, file: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/spikes/2026-11-03-agent-telemetry/fixtures")
        .join(dir)
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str::<Value>(&text).unwrap()["body"].clone()
}

const PROTOBUF: &str = "application/x-protobuf";
const JSON: &str = "application/json";

/// The body of `request` in the encoding of `content_type`.
fn body_of(signal: Signal, request: &Value, content_type: &str) -> Vec<u8> {
    if content_type == PROTOBUF {
        encode::json_to_protobuf(signal, request)
    } else {
        serde_json::to_vec(request).unwrap()
    }
}

/// An HTTP answer.
#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// What identifies the answer, without what changes from one to the next (`date`).
    fn shape(&self) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let headers = self
            .headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("date"))
            .cloned()
            .collect();
        (self.status, headers, self.body.clone())
    }
}

/// One request on its own connection. `connection: close` makes the server end it, so the
/// answer is everything up to the end of the stream.
struct Request {
    method: &'static str,
    path: String,
    host: Option<String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    chunked: bool,
    /// Declared `Content-Length` when it differs from the body (a body that is never sent).
    declared: Option<usize>,
}

impl Request {
    fn post(path: &str, body: Vec<u8>, content_type: &str) -> Self {
        Self {
            method: "POST",
            path: path.to_owned(),
            host: None,
            headers: vec![("content-type".into(), content_type.into())],
            body,
            chunked: false,
            declared: None,
        }
    }

    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn host(mut self, host: &str) -> Self {
        self.host = Some(host.into());
        self
    }

    fn bytes(&self, port: u16) -> Vec<u8> {
        let mut out = Vec::new();
        write!(out, "{} {} HTTP/1.1\r\n", self.method, self.path).unwrap();
        let host = self
            .host
            .clone()
            .unwrap_or_else(|| format!("127.0.0.1:{port}"));
        write!(out, "host: {host}\r\nconnection: close\r\n").unwrap();
        for (k, v) in &self.headers {
            write!(out, "{k}: {v}\r\n").unwrap();
        }
        if self.chunked {
            out.extend_from_slice(b"transfer-encoding: chunked\r\n\r\n");
            for chunk in self.body.chunks(4096) {
                write!(out, "{:x}\r\n", chunk.len()).unwrap();
                out.extend_from_slice(chunk);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"0\r\n\r\n");
        } else {
            write!(
                out,
                "content-length: {}\r\n\r\n",
                self.declared.unwrap_or(self.body.len())
            )
            .unwrap();
            out.extend_from_slice(&self.body);
        }
        out
    }

    async fn send(&self, port: u16) -> Reply {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // A refusal may arrive before the whole body is written (that is the point of some
        // tests): a write the server cut short is not the test's failure.
        let _ = stream.write_all(&self.bytes(port)).await;
        let mut answer = Vec::new();
        let _ = timeout(WAIT, stream.read_to_end(&mut answer))
            .await
            .unwrap();
        parse(&answer)
    }
}

fn parse(answer: &[u8]) -> Reply {
    let split = answer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no answer: {:?}", String::from_utf8_lossy(answer)));
    let head = std::str::from_utf8(&answer[..split]).unwrap();
    let mut lines = head.lines();
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: answer[split + 4..].to_vec(),
    }
}

// The session side -----------------------------------------------------------------------------

static NEXT: AtomicU32 = AtomicU32::new(0);

/// A terminal session that wrote the endpoint and token it was given to a file.
struct Reporter {
    id: SessionId,
    endpoint: String,
    token: String,
}

/// The agent says it is there (a state report, as every integration's first report is): a terminal
/// session's insight can be read once it has an agent.
async fn present(client: &Client, session: SessionId, agent: &str) {
    let request = AgentReportRequest {
        session_id: session,
        source: "test-hook".into(),
        agent: agent.into(),
        state: Some(AgentState::Idle),
        message: None,
        seq: 1,
        agent_session_id: None,
        resume_argv: None,
        node: None,
        pid: None,
        insight: None,
    };
    let applied: AgentApplied = client.call(Wire::AgentReport(request)).await.unwrap();
    assert!(applied.applied);
}

impl Reporter {
    /// [`start`](Self::start), and the agent `agent` is there.
    async fn start_as(t: &TestDaemon, client: &Client, agent: &str) -> Self {
        let reporter = Self::start(t, client).await;
        present(client, reporter.id, agent).await;
        reporter
    }

    async fn start(t: &TestDaemon, client: &Client) -> Self {
        let out = t
            ._dir
            .path()
            .join(format!("otlp-env-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        let script = format!(
            "printf '%s %s\\n' \"$HUTAN_OTLP_ENDPOINT\" \"$HUTAN_OTLP_TOKEN\" > {out}.tmp && mv {out}.tmp {out}; exec sleep 600",
            out = out.display()
        );
        let session = common::create(client, sh(&script)).await;
        let text = timeout(WAIT, async {
            loop {
                if let Ok(text) = std::fs::read_to_string(&out) {
                    break text;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the session never wrote its environment");
        let mut words = text.split_whitespace();
        Self {
            id: session.id,
            endpoint: words.next().unwrap_or_default().to_owned(),
            token: words.next().unwrap_or_default().to_owned(),
        }
    }

    /// The path of `/s/<token>/v1/<signal>`.
    fn path(&self, signal: Signal) -> String {
        format!("/s/{}/v1/{}", self.token, signal.as_str())
    }

    fn post(&self, signal: Signal, request: &Value, content_type: &str) -> Request {
        Request::post(
            &self.path(signal),
            body_of(signal, request, content_type),
            content_type,
        )
    }
}

async fn enable(client: &Client, agents: &[&str]) {
    for agent in agents {
        client
            .daemon_setting_set(&format!("telemetry.agents.{agent}"), Some(json!(true)))
            .await
            .unwrap();
    }
}

async fn status(client: &Client) -> TelemetryStatus {
    client.telemetry_status().await.unwrap()
}

async fn port_of(client: &Client) -> u16 {
    status(client).await.port.expect("the receiver has a port")
}

async fn insight(client: &Client, session: SessionId) -> AgentInsight {
    client.agent_insight(session, None).await.unwrap().insight
}

/// Waits until the insight of `session` satisfies `pred`; the reports are mapped after the
/// answer, so a test waits on the state, never on time.
async fn wait_insight(
    client: &Client,
    session: SessionId,
    what: &str,
    pred: impl Fn(&AgentInsight) -> bool,
) -> AgentInsight {
    timeout(WAIT, async {
        loop {
            let now = insight(client, session).await;
            if pred(&now) {
                return now;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the insight never showed {what}"))
}

fn source_of(insight: &AgentInsight, block: InsightBlock) -> Option<InsightSourceKind> {
    insight
        .sources
        .iter()
        .find(|s| s.block == block)
        .map(|s| s.source)
}

fn fast_idle(config: &mut hutan_daemon::DaemonConfig) {
    config.telemetry.idle_close = Duration::from_millis(150);
}

// Off, on, loopback --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn off_means_no_socket_and_no_environment_then_on_means_a_loopback_listener() {
    let t = TestDaemon::start().await;
    let client = t.connect().await;
    let off = status(&client).await;
    assert_eq!(off.listener, ListenerState::Off);
    assert_eq!(off.port, None);
    assert!(off.agents.iter().all(|a| !a.enabled));
    assert!(off.agents.iter().any(|a| a.id == "claude"));
    // A session started while it is off has no endpoint and no token.
    let before = Reporter::start(&t, &client).await;
    assert_eq!((before.endpoint.as_str(), before.token.as_str()), ("", ""));

    enable(&client, &["claude"]).await;
    let on = status(&client).await;
    assert_eq!(on.listener, ListenerState::Listening);
    let port = on.port.unwrap();
    assert!(
        (49152..=65535).contains(&port),
        "picked in the ephemeral range: {port}"
    );
    assert!(on.agents.iter().any(|a| a.id == "claude" && a.enabled));
    // The picked port was written to daemon.toml, so that it survives a restart.
    let file = std::fs::read_to_string(&t.paths.config_file).unwrap();
    assert!(file.contains(&format!("port = {port}")), "{file}");
    assert!(file.contains("claude = true"), "{file}");

    // Reachable on the loopback address; not on the IPv6 loopback, so not a wildcard bind.
    TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    assert!(TcpStream::connect(("::1", port)).await.is_err());

    // A session started now has both variables, and they agree with each other.
    let after = Reporter::start(&t, &client).await;
    assert_eq!(after.token.len(), 32);
    assert_eq!(
        after.endpoint,
        format!("http://127.0.0.1:{port}/s/{}", after.token)
    );

    // Closed with the last agent.
    client
        .daemon_setting_set("telemetry.agents.claude", Some(json!(false)))
        .await
        .unwrap();
    assert_eq!(status(&client).await.listener, ListenerState::Off);
    timeout(WAIT, async {
        while TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the port was never closed");
    let again = Reporter::start(&t, &client).await;
    assert_eq!((again.endpoint.as_str(), again.token.as_str()), ("", ""));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_picked_port_is_kept_across_a_restart() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", |_| {}).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    drop(client);
    let t = t.restart().await;
    let client = t.connect().await;
    let again = status(&client).await;
    assert_eq!(again.listener, ListenerState::Listening);
    assert_eq!(again.port, Some(port));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_persisted_port_leaves_the_receiver_down_and_visible_never_elsewhere() {
    let squatter = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = squatter.local_addr().unwrap().port();
    let toml = format!("[telemetry]\nport = {port}\n[telemetry.agents]\ncodex = true\n");
    let t = TestDaemon::start_with(&toml, |_| {}).await;
    let client = t.connect().await;
    let down = status(&client).await;
    assert_eq!(down.listener, ListenerState::Down);
    assert_eq!(down.port, Some(port));
    let reason = down.down_reason.unwrap();
    assert!(reason.contains(&format!("127.0.0.1:{port}")), "{reason}");
    // The sessions it starts get no endpoint: nothing listens for them.
    let session = Reporter::start(&t, &client).await;
    assert_eq!(session.endpoint, "");
    // Applying settings again does not move it: it tries the same port.
    enable(&client, &["claude"]).await;
    let still = status(&client).await;
    assert_eq!(
        (still.listener, still.port),
        (ListenerState::Down, Some(port))
    );
    // The port frees up: the next application opens it, on the same port.
    drop(squatter);
    enable(&client, &["omp"]).await;
    let up = status(&client).await;
    assert_eq!(
        (up.listener, up.port),
        (ListenerState::Listening, Some(port))
    );
    t.stop().await;
}

// Who may post, and as whom --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_or_wrong_token_is_refused_with_one_and_the_same_answer() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", |_| {}).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let session = Reporter::start(&t, &client).await;
    let body = serde_json::to_vec(&recording("claude-code-2.1.316", "logs.json")).unwrap();

    let refusals = [
        // No token anywhere.
        Request::post("/v1/logs", body.clone(), JSON),
        // A wrong token in the path, in each header form, and one that is nearly right.
        Request::post(
            &format!("/s/{}/v1/logs", "0".repeat(32)),
            body.clone(),
            JSON,
        ),
        Request::post("/v1/logs", body.clone(), JSON).header("x-hutan-token", &"1".repeat(32)),
        Request::post("/v1/logs", body.clone(), JSON).header("authorization", "Bearer nope"),
        Request::post(
            &format!("/s/{}x/v1/logs", session.token),
            body.clone(),
            JSON,
        ),
        Request::post(
            &format!("/s/{}/v1/logs", &session.token[..31]),
            body.clone(),
            JSON,
        ),
        // The token where it does not belong: in the query string, or in the path's middle.
        Request::post(
            &format!("/v1/logs?token={}", session.token),
            body.clone(),
            JSON,
        ),
        Request::post(&format!("/x/{}/v1/logs", session.token), body.clone(), JSON),
    ];
    let mut shapes = Vec::new();
    for request in &refusals {
        let reply = request.send(port).await;
        assert_eq!(reply.status, 401, "{:?}", request.path);
        shapes.push(reply.shape());
    }
    assert!(
        shapes.windows(2).all(|w| w[0] == w[1]),
        "the refusals differ: {shapes:?}"
    );

    // The right token, three ways, is answered 200 with the empty response.
    for request in [
        Request::post(&session.path(Signal::Logs), body.clone(), JSON),
        Request::post("/v1/logs", body.clone(), JSON).header("x-hutan-token", &session.token),
        Request::post("/v1/logs", body.clone(), JSON)
            .header("authorization", &format!("Bearer {}", session.token)),
    ] {
        let reply = request.send(port).await;
        assert_eq!(reply.status, 200, "{:?}", request.path);
        assert_eq!(reply.body, b"{}");
        assert_eq!(reply.header("content-type"), Some(JSON));
    }
    // The protobuf answer is the signal's empty message: zero bytes.
    let reply = session
        .post(
            Signal::Logs,
            &recording("claude-code-2.1.316", "logs.json"),
            PROTOBUF,
        )
        .send(port)
        .await;
    assert_eq!((reply.status, reply.body.len()), (200, 0));
    assert_eq!(reply.header("content-type"), Some(PROTOBUF));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_cannot_report_as_another_whatever_its_payload_says() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", fast_idle).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let (a, b) = (
        Reporter::start_as(&t, &client, "claude").await,
        Reporter::start_as(&t, &client, "claude").await,
    );
    // A's token, with a payload that names B in every label a payload has.
    let mut logs = recording("claude-code-2.1.316", "logs.json");
    let label = |key: &str| json!({"key": key, "value": {"stringValue": b.id.to_string()}});
    logs["resourceLogs"][0]["resource"]["attributes"]
        .as_array_mut()
        .unwrap()
        .extend([
            label("session.id"),
            label("hutan.session"),
            label("conversation.id"),
        ]);
    for record in logs["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
        .as_array_mut()
        .unwrap()
    {
        record["attributes"]
            .as_array_mut()
            .unwrap()
            .extend([label("session.id"), label("hutan.session")]);
    }
    assert_eq!(
        a.post(Signal::Logs, &logs, JSON).send(port).await.status,
        200
    );
    let mine = wait_insight(&client, a.id, "A's tokens", |i| {
        i.totals.as_ref().is_some_and(|t| t.input == Some(2_700))
    })
    .await;
    assert_eq!(
        source_of(&mine, InsightBlock::Turns),
        Some(InsightSourceKind::Otlp)
    );
    // B heard nothing: no block, no source.
    let theirs = insight(&client, b.id).await;
    assert!(
        theirs.sources.is_empty() && theirs.totals.is_none(),
        "{theirs:?}"
    );
    // The status lists A only.
    let listed = status(&client).await;
    assert_eq!(listed.sessions.len(), 1);
    assert_eq!(listed.sessions[0].session_id, a.id);
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_sessions_token_stops_working() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", |_| {}).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let session = Reporter::start(&t, &client).await;
    let body = recording("claude-code-2.1.316", "logs.json");
    assert_eq!(
        session
            .post(Signal::Logs, &body, JSON)
            .send(port)
            .await
            .status,
        200
    );
    client.session_close(session.id, true).await.unwrap();
    // Closed: the token is gone, and the refusal is the same as for any other wrong token.
    let reply = session.post(Signal::Logs, &body, JSON).send(port).await;
    assert_eq!(reply.status, 401);
    t.stop().await;
}

// What is checked before the body ----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn method_host_content_type_signal_and_size_are_refused_before_the_body_is_read() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", |_| {}).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let s = Reporter::start(&t, &client).await;
    let ok = serde_json::to_vec(&recording("claude-code-2.1.316", "logs.json")).unwrap();
    let path = s.path(Signal::Logs);

    let mut get = Request::post(&path, vec![], JSON);
    get.method = "GET";
    assert_eq!(get.send(port).await.status, 405);
    // DNS rebinding: a name that resolves to the loopback is not a host of this receiver.
    assert_eq!(
        Request::post(&path, ok.clone(), JSON)
            .host("evil.example")
            .send(port)
            .await
            .status,
        421
    );
    assert_eq!(
        Request::post(&path, ok.clone(), JSON)
            .host(&format!("127.0.0.1:{}", port.wrapping_add(1)))
            .send(port)
            .await
            .status,
        421
    );
    assert_eq!(
        Request::post(&path, ok.clone(), JSON)
            .host(&format!("localhost:{port}"))
            .send(port)
            .await
            .status,
        200
    );
    // What a web page can send without a preflight is none of the OTLP types.
    for simple in [
        "text/plain",
        "application/x-www-form-urlencoded",
        "multipart/form-data",
    ] {
        assert_eq!(
            Request::post(&path, ok.clone(), simple)
                .send(port)
                .await
                .status,
            415,
            "{simple}"
        );
    }
    assert_eq!(
        Request::post(&path, ok.clone(), JSON)
            .header("content-encoding", "br")
            .send(port)
            .await
            .status,
        415
    );
    // The signal is checked after the token: an unknown one is 404 for a valid session.
    assert_eq!(
        Request::post(&format!("/s/{}/v1/profiles", s.token), ok.clone(), JSON)
            .send(port)
            .await
            .status,
        404
    );

    // A body over 1 MiB: refused on its declared size, the body never sent.
    let mut huge = Request::post(&path, vec![], JSON);
    huge.declared = Some(2 * 1024 * 1024);
    assert_eq!(huge.send(port).await.status, 413);
    // And one that does not say how long it is, sent chunked: refused when it passes the limit.
    let mut chunked = Request::post(&path, vec![b' '; 1024 * 1024 + 1], JSON);
    chunked.chunked = true;
    assert_eq!(chunked.send(port).await.status, 413);
    // A header block over 8 KiB.
    let big_header = Request::post(&path, ok.clone(), JSON).header("x-pad", &"a".repeat(9 * 1024));
    assert_eq!(big_header.send(port).await.status, 431);

    // Everything above was counted as refused; what was accepted was answered 200.
    let counters = status(&client).await.counters;
    assert!(counters.rejected >= 10, "{counters:?}");
    assert!(counters.accepted >= 1, "{counters:?}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_body_without_a_length_and_a_gzip_body_are_read() {
    let t = TestDaemon::start_with("[telemetry.agents]\nother = true\n", |_| {}).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let s = Reporter::start_as(&t, &client, "my-agent").await;
    let traces = json!({"resourceSpans": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "my-agent"}}]},
        "scopeSpans": [{"spans": [{
            "spanId": "00000000000000aa", "startTimeUnixNano": "1000000000", "endTimeUnixNano": "2000000000",
            "attributes": [
                {"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "100"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "7"}}
            ]}, {
            "spanId": "00000000000000ab", "startTimeUnixNano": "1000000000", "endTimeUnixNano": "2000000000",
            "attributes": [{"key": "gen_ai.operation.name", "value": {"stringValue": "invoke_agent"}}]
        }]}]
    }]});
    // Chunked, no Content-Length (what OMP, Gemini, Qwen and OpenCode's traces send).
    let mut chunked = s.post(Signal::Traces, &traces, JSON);
    chunked.chunked = true;
    assert_eq!(chunked.send(port).await.status, 200);
    let first = wait_insight(&client, s.id, "the chunked request", |i| {
        i.totals.as_ref().is_some_and(|t| t.input == Some(100))
    })
    .await;
    assert_eq!(first.totals.unwrap().turns, 1);

    // The same, gzipped, with another span id (the first would count as a retry).
    let mut again = serde_json::to_string(&traces).unwrap();
    again = again
        .replace("00000000000000aa", "00000000000000bb")
        .replace("00000000000000ab", "00000000000000bc");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(again.as_bytes()).unwrap();
    let gzipped = encoder.finish().unwrap();
    let reply = Request::post(&s.path(Signal::Traces), gzipped, JSON)
        .header("content-encoding", "gzip")
        .send(port)
        .await;
    assert_eq!(reply.status, 200);
    wait_insight(&client, s.id, "the gzipped request", |i| {
        i.totals.as_ref().is_some_and(|t| t.input == Some(200))
    })
    .await;

    // A bomb is accepted on the wire (it is small) and dropped when it does not inflate within
    // the limit; the daemon goes on.
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let block = vec![0_u8; 1024 * 1024];
    for _ in 0..16 {
        encoder.write_all(&block).unwrap();
    }
    let bomb = encoder.finish().unwrap();
    let reply = Request::post(&s.path(Signal::Traces), bomb, JSON)
        .header("content-encoding", "gzip")
        .send(port)
        .await;
    assert_eq!(reply.status, 200);
    timeout(WAIT, async {
        while status(&client).await.counters.dropped == 0 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the bomb was never counted as dropped");
    t.stop().await;
}

// Limits -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_session_over_its_request_rate_or_its_bytes_is_told_to_wait() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", |config| {
        // No refill: the buckets only drain, so the test needs no clock.
        config.telemetry.requests = hutan_daemon::telemetry::Rate {
            burst: 3.0,
            per_sec: 0.0,
        };
        config.telemetry.bytes = hutan_daemon::telemetry::Rate {
            burst: 4000.0,
            per_sec: 0.0,
        };
    })
    .await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let (busy, other) = (
        Reporter::start(&t, &client).await,
        Reporter::start(&t, &client).await,
    );
    let small = json!({"resourceLogs": []});
    let send = |s: &Reporter| s.post(Signal::Logs, &small, JSON);
    for _ in 0..3 {
        assert_eq!(send(&busy).send(port).await.status, 200);
    }
    let limited = send(&busy).send(port).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.header("retry-after"), Some("1"));
    // The limit is the session's own: another session is not slowed by it.
    assert_eq!(send(&other).send(port).await.status, 200);

    // Bytes: a 3 000-byte body fits once in a 4 000-byte bucket, and not twice.
    let padded = json!({"resourceLogs": [], "padding": "x".repeat(3000)});
    assert_eq!(
        other
            .post(Signal::Logs, &padded, JSON)
            .send(port)
            .await
            .status,
        200
    );
    assert_eq!(
        other
            .post(Signal::Logs, &padded, JSON)
            .send(port)
            .await
            .status,
        429
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_four_requests_of_a_session_and_sixteen_connections_at_once() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", |_| {}).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let s = Reporter::start(&t, &client).await;
    // Four requests that announce a body and never send it hold their session's slots (until
    // the 2 s body timeout).
    let mut held = Vec::new();
    for _ in 0..4 {
        let mut stalled = Request::post(&s.path(Signal::Logs), vec![], JSON);
        stalled.declared = Some(100);
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream.write_all(&stalled.bytes(port)).await.unwrap();
        held.push(stream);
    }
    // A fifth is told to wait as soon as the four hold their slots: the server takes them one
    // by one, so the test asks until it is told.
    let fifth = s.post(Signal::Logs, &json!({"resourceLogs": []}), JSON);
    let told = timeout(WAIT, async {
        loop {
            let reply = fifth.send(port).await;
            if reply.status == 429 {
                return reply;
            }
            assert_eq!(reply.status, 200);
        }
    })
    .await
    .expect("a fifth request of the session was never refused");
    assert_eq!(told.header("retry-after"), Some("1"));
    drop(held);

    // Seventeen connections that send nothing: the server keeps sixteen and closes the rest.
    let mut idle = Vec::new();
    for _ in 0..17 {
        idle.push(TcpStream::connect(("127.0.0.1", port)).await.unwrap());
    }
    timeout(WAIT, async {
        loop {
            for stream in &mut idle {
                let mut byte = [0_u8; 1];
                // Closed by the server, or reset: not one of the sixteen it keeps.
                if let Ok(Ok(0) | Err(_)) =
                    timeout(Duration::from_millis(20), stream.read(&mut byte)).await
                {
                    return;
                }
            }
        }
    })
    .await
    .expect("no connection past the sixteenth was closed");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_mapper_never_slows_the_answer_and_a_full_queue_says_429() {
    let gate = Arc::new(Semaphore::new(0));
    let held = gate.clone();
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", move |config| {
        config.telemetry.queue = 4;
        config.telemetry.mapper_gate = Some(held);
        config.telemetry.requests = hutan_daemon::telemetry::Rate {
            burst: 1e6,
            per_sec: 1e6,
        };
    })
    .await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let s = Reporter::start_as(&t, &client, "claude").await;
    let logs = recording("claude-code-2.1.316", "logs.json");

    // The mapper holds one request and the queue four: six requests in, every answer under
    // 50 ms (the median; a loaded machine may delay one) and the sixth is told to wait.
    let mut times = Vec::new();
    let mut statuses = Vec::new();
    for _ in 0..6 {
        let begun = Instant::now();
        let reply = s.post(Signal::Logs, &logs, PROTOBUF).send(port).await;
        times.push(begun.elapsed());
        statuses.push(reply.status);
    }
    times.sort();
    assert!(
        times[times.len() / 2] < Duration::from_millis(50),
        "{times:?}"
    );
    assert!(*times.last().unwrap() < Duration::from_secs(2), "{times:?}");
    assert_eq!(
        statuses.iter().filter(|s| **s == 200).count(),
        5,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 429).count(),
        1,
        "{statuses:?}"
    );
    let counters = status(&client).await.counters;
    assert_eq!(
        (counters.accepted, counters.dropped),
        (5, 1),
        "{counters:?}"
    );

    // Nothing was mapped meanwhile; released, the mapper catches up.
    assert!(insight(&client, s.id).await.sources.is_empty());
    gate.add_permits(100);
    wait_insight(&client, s.id, "the held requests", |i| {
        !i.sources.is_empty()
    })
    .await;
    t.stop().await;
}

// What is mapped -----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn claude_codes_recorded_logs_become_the_insight_of_its_session_in_both_encodings() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", fast_idle).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    for (n, content_type) in [PROTOBUF, JSON].into_iter().enumerate() {
        let retries = n as u64 + 1;
        let s = Reporter::start_as(&t, &client, "claude").await;
        let logs = recording("claude-code-2.1.316", "logs.json");
        assert_eq!(
            s.post(Signal::Logs, &logs, content_type)
                .send(port)
                .await
                .status,
            200
        );
        // The logs never say a turn ended: the idle close does (150 ms here), and only then do
        // its tokens count.
        let now = wait_insight(&client, s.id, "the ended turn", |i| {
            i.totals
                .as_ref()
                .is_some_and(|t| t.turns == 1 && t.input.is_some())
        })
        .await;
        let totals = now.totals.clone().unwrap();
        assert_eq!(
            (
                totals.input,
                totals.cache_read,
                totals.cache_write,
                totals.output
            ),
            (Some(2_700), Some(3_000), Some(3_000), Some(42)),
            "{content_type}"
        );
        assert!((totals.cost_usd.unwrap() - 0.02724).abs() < 1e-9);
        let tools = now.tools.clone().unwrap();
        assert_eq!((tools.calls, tools.failed), (1, 0));
        assert_eq!(tools.by_name[0].name, "Read");
        assert_eq!(
            source_of(&now, InsightBlock::Turns),
            Some(InsightSourceKind::Otlp)
        );
        assert_eq!(
            source_of(&now, InsightBlock::Tools),
            Some(InsightSourceKind::Otlp)
        );
        assert_eq!(now.harness.as_deref(), Some("claude"));
        // The session shows in the status with the agent's version.
        let listed = status(&client).await;
        let mine = listed
            .sessions
            .iter()
            .find(|x| x.session_id == s.id)
            .unwrap();
        assert_eq!(mine.agent, "claude");
        assert_eq!(mine.service_version.as_deref(), Some("2.1.316"));
        assert_eq!(mine.model.as_deref(), Some("claude-opus-5-5"));
        assert!(mine.reports >= 1);
        // A retried request adds nothing: the mapper reaches it (it counts as ignored) and the
        // totals stay what they were.
        assert_eq!(
            s.post(Signal::Logs, &logs, content_type)
                .send(port)
                .await
                .status,
            200
        );
        wait_status(&client, |st| st.counters.ignored >= retries).await;
        assert_eq!(
            insight(&client, s.id).await.totals.unwrap().input,
            Some(2_700)
        );
    }
    t.stop().await;
}

async fn wait_status(client: &Client, pred: impl Fn(&TelemetryStatus) -> bool) -> TelemetryStatus {
    timeout(WAIT, async {
        loop {
            let now = status(client).await;
            if pred(&now) {
                return now;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the status never showed it")
}

#[tokio::test(flavor = "multi_thread")]
async fn codexs_recorded_logs_keep_the_cache_inside_the_input() {
    let t = TestDaemon::start_with("[telemetry.agents]\ncodex = true\n", fast_idle).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    for content_type in [JSON, PROTOBUF] {
        let s = Reporter::start_as(&t, &client, "codex").await;
        let logs = recording("codex-0.166.0", "logs-turn.json");
        assert_eq!(
            s.post(Signal::Logs, &logs, content_type)
                .send(port)
                .await
                .status,
            200
        );
        let now = wait_insight(&client, s.id, "the ended turn", |i| {
            i.totals
                .as_ref()
                .is_some_and(|t| t.turns == 1 && t.input.is_some())
        })
        .await;
        let totals = now.totals.unwrap();
        assert_eq!(
            (
                totals.input,
                totals.cache_read,
                totals.output,
                totals.reasoning
            ),
            (Some(4_100), Some(1_500), Some(55), Some(5)),
            "{content_type}"
        );
        // Codex reports no cost: absent, never zero.
        assert_eq!(totals.cost_usd, None);
        // Codex's reducer counts the cache inside the input: 1 500 of 4 100.
        assert!((totals.cache_hit_ratio.unwrap() - 1500.0 / 4100.0).abs() < 1e-9);
        assert_eq!(now.tools.unwrap().by_name[0].name, "exec_command");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn genai_spans_of_a_generic_agent_become_insight_in_both_encodings() {
    let t =
        TestDaemon::start_with("[telemetry.agents]\ncopilot = true\ngoose = true\n", |_| {}).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    for (dir, content_type) in [
        ("copilot-cli-1.0.105", PROTOBUF),
        ("goose-1.57.0", JSON),
        ("copilot-cli-1.0.105", JSON),
    ] {
        let s = Reporter::start_as(&t, &client, dir.split('-').next().unwrap()).await;
        let spans = recording(dir, "traces.json");
        assert_eq!(
            s.post(Signal::Traces, &spans, content_type)
                .send(port)
                .await
                .status,
            200
        );
        let now = wait_insight(&client, s.id, "the ended turn", |i| {
            i.totals.as_ref().is_some_and(|t| t.turns == 1)
        })
        .await;
        let totals = now.totals.unwrap();
        // Input without the cache; Goose's wrapping `chat` span is not counted twice.
        assert_eq!(
            (totals.input, totals.cache_read, totals.output),
            (Some(8_200), Some(14_000), Some(52)),
            "{dir} {content_type}"
        );
        assert_eq!(now.tools.unwrap().calls, 1);
    }
    // An agent the user did not enable is not read: nothing of OMP's reaches its session.
    let s = Reporter::start_as(&t, &client, "omp").await;
    let omp = recording("omp-18.14.1", "traces.json");
    assert_eq!(
        s.post(Signal::Traces, &omp, JSON).send(port).await.status,
        200
    );
    wait_status(&client, |st| st.counters.ignored >= 1).await;
    assert!(insight(&client, s.id).await.sources.is_empty());
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn content_attributes_are_never_stored_anywhere_the_daemon_can_show() {
    let t = TestDaemon::start_with(
        "[telemetry.agents]\ncodex = true\nclaude = true\nother = true\n",
        fast_idle,
    )
    .await;
    let client = t.connect().await;
    let port = port_of(&client).await;

    // The real Codex recording carries the tool's arguments and output; a payload built to
    // leak carries every content attribute of the spike's list, on the resource and the records.
    let secret = "TOP-SECRET-7f3a";
    let leak = |key: &str| json!({"key": key, "value": {"stringValue": secret}});
    let keys = [
        "prompt",
        "arguments",
        "output",
        "function_args",
        "ai.prompt",
        "gen_ai.input.messages",
        "gen_ai.system_instructions",
        "process.command_args",
        "user.email",
        "tool_input",
        "gen_ai.tool.call.arguments",
    ];
    let leaking_attrs: Vec<Value> = keys.iter().map(|k| leak(k)).collect();
    let mut generic = json!({"resourceSpans": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "leaky"}}]},
        "scopeSpans": [{"spans": [{
            "spanId": "00000000000000cc", "startTimeUnixNano": "1", "endTimeUnixNano": "2000001",
            "attributes": [
                {"key": "gen_ai.operation.name", "value": {"stringValue": "execute_tool"}},
                {"key": "gen_ai.tool.name", "value": {"stringValue": "bash"}}
            ]
        }]}]
    }]});
    for (path, items) in [
        ("/resourceSpans/0/resource/attributes", &leaking_attrs),
        (
            "/resourceSpans/0/scopeSpans/0/spans/0/attributes",
            &leaking_attrs,
        ),
    ] {
        generic
            .pointer_mut(path)
            .unwrap()
            .as_array_mut()
            .unwrap()
            .extend(items.clone());
    }
    let mut claude = recording("claude-code-2.1.316", "logs.json");
    for record in claude["resourceLogs"][0]["scopeLogs"][0]["logRecords"]
        .as_array_mut()
        .unwrap()
    {
        record["attributes"]
            .as_array_mut()
            .unwrap()
            .extend(leaking_attrs.clone());
    }
    let mut shown = Vec::new();
    for (n, (signal, body, harness)) in [
        (
            Signal::Logs,
            recording("codex-0.166.0", "logs-turn.json"),
            "codex",
        ),
        (Signal::Logs, claude, "claude"),
        // An unknown program goes under its own name (`service.name`), not under "other".
        (Signal::Traces, generic, "leaky"),
    ]
    .into_iter()
    .enumerate()
    {
        let retries = n as u64 + 1;
        let s = Reporter::start_as(&t, &client, harness).await;
        for content_type in [JSON, PROTOBUF] {
            assert_eq!(
                s.post(signal, &body, content_type).send(port).await.status,
                200
            );
        }
        // The second is the first again: the mapper has reached both when it counts the second
        // as ignored (nothing new in it).
        wait_status(&client, |st| st.counters.ignored >= retries).await;
        let now = insight(&client, s.id).await;
        assert_eq!(now.harness.as_deref(), Some(harness));
        assert!(now.tools.is_some(), "{harness}: {now:?}");
        shown.push(serde_json::to_string(&now).unwrap());
    }
    // Nothing the daemon can show contains a payload's text: the insights, the status, the
    // settings.
    shown.push(serde_json::to_string(&status(&client).await).unwrap());
    shown.push(serde_json::to_string(&client.daemon_settings().await.unwrap()).unwrap());
    let shown = shown.join("\n");
    for canary in [secret, "echo hi", "Chunk ID", "REDACTED"] {
        assert!(!shown.contains(canary), "{canary} reached {shown}");
    }
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_token_is_in_no_status_no_setting_and_no_insight() {
    let t = TestDaemon::start_with("[telemetry.agents]\nclaude = true\n", fast_idle).await;
    let client = t.connect().await;
    let port = port_of(&client).await;
    let s = Reporter::start_as(&t, &client, "claude").await;
    let logs = recording("claude-code-2.1.316", "logs.json");
    assert_eq!(
        s.post(Signal::Logs, &logs, JSON).send(port).await.status,
        200
    );
    wait_insight(&client, s.id, "the insight", |i| i.totals.is_some()).await;
    let everything = [
        serde_json::to_string(&status(&client).await).unwrap(),
        serde_json::to_string(&client.daemon_settings().await.unwrap()).unwrap(),
        serde_json::to_string(&insight(&client, s.id).await).unwrap(),
        serde_json::to_string(
            &client
                .call::<hutan_protocol::messages::SessionList>(
                    hutan_protocol::messages::Request::SessionList(Default::default()),
                )
                .await
                .unwrap(),
        )
        .unwrap(),
        std::fs::read_to_string(&t.paths.config_file).unwrap(),
    ]
    .join("\n");
    assert!(!everything.contains(&s.token), "the token leaked");
    // Nor on disk: not in the database (a closed WAL has no page of it), not in any file of the home.
    let mut found = Vec::new();
    fn scan(dir: &Path, token: &str, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan(&path, token, found);
            } else if let Ok(bytes) = std::fs::read(&path)
                && bytes.windows(token.len()).any(|w| w == token.as_bytes())
                && !path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("otlp-env-"))
            {
                found.push(path.display().to_string());
            }
        }
    }
    scan(t._dir.path(), &s.token, &mut found);
    assert!(found.is_empty(), "the token is on disk: {found:?}");
    t.stop().await;
}
