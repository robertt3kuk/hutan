//! The JSON-RPC loop of `hutan mcp` (MCP over stdio: one JSON message per line) and the
//! execution of tool calls against the daemon.
//!
//! Hand-rolled on purpose: the protocol surface used here (initialize, ping, tools/list,
//! tools/call, cancellation) is small, and an SDK crate would be a large dependency for it.

use std::collections::HashMap;
use std::sync::Arc;

use hutan_client::{Client, ClientError, ConnectOptions};
use hutan_protocol::messages::{ClientInfo, ClientKind, Request};
use hutan_protocol::{ErrorBody, ErrorCode};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};
use tokio::task::AbortHandle;

use super::guide;
use super::table::{self, Call, Tool};

/// The newest protocol version this server speaks, and the older ones it also answers.
const LATEST: &str = "2025-06-18";
const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// A tool result longer than this is cut (a model cannot use more, and hosts refuse it).
const MAX_TEXT_BYTES: usize = 100_000;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;

/// How the server reaches the daemon.
#[derive(Debug, Clone)]
pub struct Target {
    pub socket: Option<std::path::PathBuf>,
}

/// One server: the lazily made daemon connection and the requests in flight.
struct Server {
    target: Target,
    client: Mutex<Option<Client>>,
}

impl Server {
    fn options(&self) -> ConnectOptions {
        ConnectOptions {
            socket: self.target.socket.clone(),
            autostart: None,
            client: ClientInfo {
                kind: ClientKind::Other,
                version: format!("mcp/{}", env!("CARGO_PKG_VERSION")),
                origin: crate::commands::session_origin(),
            },
        }
    }

    /// The connection, made now if there is none.
    async fn client(&self) -> Result<Client, ClientError> {
        let mut slot = self.client.lock().await;
        if let Some(client) = &*slot {
            return Ok(client.clone());
        }
        let client = Client::connect(self.options()).await?;
        *slot = Some(client.clone());
        Ok(client)
    }

    async fn forget(&self) {
        *self.client.lock().await = None;
    }

    /// Runs `request`, connecting again once when the connection was lost.
    async fn call(&self, request: Request) -> Result<Value, ClientError> {
        for attempt in 0..2 {
            let client = self.client().await?;
            match client.call::<Value>(request.clone()).await {
                Err(ClientError::Disconnected) if attempt == 0 => self.forget().await,
                other => return other,
            }
        }
        Err(ClientError::Disconnected)
    }
}

/// Serves MCP on `input`/`output` until `input` ends.
pub async fn serve<R, W>(input: R, output: W, target: Target) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let server = Arc::new(Server {
        target,
        client: Mutex::new(None),
    });
    // Responses of tasks in flight and of the loop itself go through one writer, so lines
    // never interleave.
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let writer = tokio::spawn(async move {
        let mut output = output;
        while let Some(line) = out_rx.recv().await {
            if output.write_all(line.as_bytes()).await.is_err()
                || output.write_all(b"\n").await.is_err()
                || output.flush().await.is_err()
            {
                break;
            }
        }
    });
    let in_flight: Arc<Mutex<HashMap<String, AbortHandle>>> = Arc::default();

    let mut lines = BufReader::new(input).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(e) => {
                send(
                    &out_tx,
                    error_response(Value::Null, PARSE_ERROR, &format!("parse error: {e}")),
                )
                .await;
                continue;
            }
        };
        let Some(object) = message.as_object() else {
            send(
                &out_tx,
                error_response(Value::Null, INVALID_REQUEST, "a message is a JSON object"),
            )
            .await;
            continue;
        };
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let id = object.get("id").cloned();
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        match (method, id) {
            // A response to a request of ours: there are none.
            (None, _) => {}
            (Some(method), None) => {
                if method == "notifications/cancelled"
                    && let Some(request_id) = params.get("requestId")
                    && let Some(handle) = in_flight.lock().await.remove(&request_id.to_string())
                {
                    handle.abort();
                }
            }
            (Some(method), Some(id)) if method == "tools/call" => {
                // A call may wait for the user's approval: it must not hold the loop up.
                let key = id.to_string();
                let server = server.clone();
                let out = out_tx.clone();
                let registry = in_flight.clone();
                let done_key = key.clone();
                let task = tokio::spawn(async move {
                    let result = call_tool(&server, params).await;
                    registry.lock().await.remove(&done_key);
                    send(&out, response(id, result)).await;
                });
                in_flight.lock().await.insert(key, task.abort_handle());
            }
            (Some(method), Some(id)) => {
                let reply = match handle(&method, params) {
                    Ok(result) => response(id, result),
                    Err((code, message)) => error_response(id, code, &message),
                };
                send(&out_tx, reply).await;
            }
        }
    }
    for (_, handle) in in_flight.lock().await.drain() {
        handle.abort();
    }
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}

async fn send(out: &mpsc::Sender<String>, message: Value) {
    let _ = out.send(message.to_string()).await;
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The requests that need no daemon.
fn handle(method: &str, params: Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked.filter(|v| SUPPORTED.contains(v)).unwrap_or(LATEST);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {
                    "name": "hutan",
                    "title": "Hutan",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": guide::MCP_INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            Ok(json!({"tools": table::tools().iter().map(describe).collect::<Vec<_>>()}))
        }
        other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
    }
}

fn describe(tool: &Tool) -> Value {
    json!({
        "name": tool.name,
        "title": tool.name.replace('_', " "),
        "description": tool.description,
        "inputSchema": (tool.schema)(),
        "annotations": {
            "readOnlyHint": tool.read_only,
            "destructiveHint": !tool.read_only,
            "openWorldHint": false,
        },
    })
}

/// A tool result: text for the model, the data as structured content when it is an object.
fn tool_result(text: String, structured: Option<Value>, is_error: bool) -> Value {
    let text = cut(text);
    let mut result = json!({"content": [{"type": "text", "text": text}], "isError": is_error});
    if let Some(structured @ Value::Object(_)) = structured
        && let Value::Object(map) = &mut result
    {
        map.insert("structuredContent".to_owned(), structured);
    }
    result
}

fn cut(mut text: String) -> String {
    if text.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n… (cut: the result is longer than this tool shows; ask for less)");
    }
    text
}

/// `tools/call`: failures of the tool are results with `isError`, so the model sees them; only
/// a call to a tool that does not exist is a protocol error.
async fn call_tool(server: &Server, params: Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return protocol_error_result("tools/call needs a tool `name`");
    };
    let Some(tool) = table::find(name) else {
        return protocol_error_result(&format!("unknown tool `{name}`"));
    };
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(arguments) => arguments.clone(),
    };
    if !arguments.is_object() {
        return tool_result("the arguments must be a JSON object".to_owned(), None, true);
    }
    match run_tool(server, tool, arguments).await {
        Ok((text, structured)) => tool_result(text, structured, false),
        Err(text) => tool_result(text, None, true),
    }
}

fn protocol_error_result(message: &str) -> Value {
    tool_result(message.to_owned(), None, true)
}

type ToolOutcome = Result<(String, Option<Value>), String>;

async fn run_tool(server: &Server, tool: &Tool, arguments: Value) -> ToolOutcome {
    match tool.call {
        Call::Request {
            method,
            shape,
            fixed,
        } => {
            let mut params = match shape {
                Some(shape) => shape(arguments).map_err(|e| format!("bad arguments: {e}"))?,
                None => arguments,
            };
            if let Value::Object(map) = &mut params {
                for (key, value) in fixed {
                    map.insert((*key).to_owned(), json!(value));
                }
            }
            // A bad argument is `bad_request` from the typed decoder, with its own words.
            let request = Request::decode(method, params)
                .map_err(|e| format!("bad arguments: {}", e.message))?;
            let result = server.call(request).await.map_err(describe_error)?;
            Ok(describe_success(result))
        }
        Call::SessionOutput => session_output(server, arguments).await,
    }
}

async fn session_output(server: &Server, arguments: Value) -> ToolOutcome {
    let session_id = arguments
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "bad arguments: `session_id` is required".to_owned())?;
    let session_id: hutan_core::SessionId = session_id
        .parse()
        .map_err(|e| format!("bad arguments: `session_id`: {e}"))?;
    let lines = arguments
        .get("lines")
        .and_then(Value::as_u64)
        .map_or(100, |n| n.clamp(1, 2000));
    let list = server
        .call(Request::SessionList(Default::default()))
        .await
        .map_err(describe_error)?;
    let sessions: hutan_protocol::messages::SessionList =
        serde_json::from_value(list).map_err(|e| format!("unexpected answer: {e}"))?;
    let session = sessions
        .sessions
        .iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| format!("not_found: session {session_id} not found"))?;
    if session.chat.is_some() {
        return Err(format!(
            "session {session_id} is a chat: use `chat_log` to read it"
        ));
    }
    let terminal = session
        .terminal_id
        .ok_or_else(|| format!("session {session_id} has no terminal"))?;
    let client = server.client().await.map_err(describe_error)?;
    let lines =
        crate::tail::terminal_tail(&client, terminal, usize::try_from(lines).unwrap_or(100))
            .await
            .map_err(describe_error)?;
    let text = if lines.is_empty() {
        "(no output yet)".to_owned()
    } else {
        lines.join("\n")
    };
    Ok((
        text,
        Some(json!({"session_id": session_id, "lines": lines})),
    ))
}

/// The result of a request, as text, and a note when it waited for the user.
fn describe_success(mut result: Value) -> (String, Option<Value>) {
    let control = result
        .as_object_mut()
        .and_then(|map| map.get("control").cloned());
    let note = control.as_ref().and_then(approval_note);
    // The decision stays in the structured result; the text carries the data and the note.
    let shown = match (&control, result.as_object()) {
        (Some(_), Some(map)) => {
            let mut map = map.clone();
            map.remove("control");
            Value::Object(map)
        }
        _ => result.clone(),
    };
    let mut text = serde_json::to_string_pretty(&shown).unwrap_or_else(|_| shown.to_string());
    if let Some(note) = note {
        text = format!("{note}\n{text}");
    }
    (text, Some(result))
}

fn approval_note(control: &Value) -> Option<String> {
    let decision = control.get("decision")?.as_str()?;
    let waited = control
        .get("waited_ms")
        .and_then(Value::as_u64)
        .map(|ms| format!(" after waiting {:.1} s", ms as f64 / 1000.0))
        .unwrap_or_default();
    Some(match decision {
        "allow_once" => format!("[The user approved this request{waited}.]"),
        "allow_session" => format!(
            "[The user approved this request{waited}, and allowed this kind of request for this session.]"
        ),
        "allow_chat" => format!(
            "[The user approved this request{waited}, and allowed this tool for this chat.]"
        ),
        "allow_workspace" => format!(
            "[The user approved this request{waited}, and allowed this tool in this workspace.]"
        ),
        "allow_grant" => {
            "[Allowed by a grant the user gave earlier (Settings → Mawas lists and revokes them).]"
                .to_owned()
        }
        other => format!("[Approval: {other}{waited}.]"),
    })
}

/// A failed request in words a model can act on.
fn describe_error(error: ClientError) -> String {
    match error {
        ClientError::Remote(body) => describe_remote(&body),
        ClientError::Disconnected => {
            "the connection to the Hutan daemon was lost; try again (is `hutand` running?)"
                .to_owned()
        }
        ClientError::Io(e) => {
            format!("cannot reach the Hutan daemon: {e}; it is probably not running")
        }
        other => other.to_string(),
    }
}

fn describe_remote(body: &ErrorBody) -> String {
    let code = serde_json::to_value(body.code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "error".to_owned());
    let decision = body
        .data
        .as_ref()
        .and_then(|data| data.pointer("/control/decision"))
        .and_then(Value::as_str);
    match (body.code, decision) {
        (ErrorCode::Forbidden, Some("deny")) => format!(
            "DENIED by the user: {} Do not try again unless the user asks you to.",
            body.message
        ),
        (ErrorCode::Forbidden, Some("expired")) => format!(
            "NOT APPROVED: {} Ask the user, then try again.",
            body.message
        ),
        (ErrorCode::Forbidden, Some("read_only")) => format!(
            "REFUSED: {} You can read, but not change, Hutan.",
            body.message
        ),
        (ErrorCode::Forbidden, Some("user_only")) => format!("REFUSED: {}", body.message),
        (ErrorCode::Forbidden, Some("advisor")) => format!(
            "REFUSED: {} Describe what you would change and let the user do it, or ask them to switch you to “Ask before acting”.",
            body.message
        ),
        _ => format!("{code}: {}", body.message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_negotiates_the_protocol_version() {
        let ok = handle("initialize", json!({"protocolVersion": "2025-03-26"})).unwrap();
        assert_eq!(ok["protocolVersion"], "2025-03-26");
        let newer = handle("initialize", json!({"protocolVersion": "2099-01-01"})).unwrap();
        assert_eq!(newer["protocolVersion"], LATEST);
        assert_eq!(ok["serverInfo"]["name"], "hutan");
        assert!(ok["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_carries_schemas_and_hints() {
        let list = handle("tools/list", Value::Null).unwrap();
        let tools = list["tools"].as_array().unwrap();
        assert_eq!(tools.len(), table::tools().len());
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert!(tool["annotations"]["readOnlyHint"].is_boolean());
        }
        let stop = tools.iter().find(|t| t["name"] == "session_stop").unwrap();
        assert_eq!(stop["annotations"]["readOnlyHint"], false);
    }

    #[test]
    fn unknown_methods_are_method_not_found() {
        let (code, _) = handle("resources/list", Value::Null).unwrap_err();
        assert_eq!(code, METHOD_NOT_FOUND);
    }

    #[test]
    fn a_waited_for_mutation_says_so() {
        let result = json!({"session": {"id": "x"}, "control": {"decision": "allow_once", "waited_ms": 12_300}});
        let (text, structured) = describe_success(result);
        assert!(
            text.starts_with("[The user approved this request after waiting 12.3 s.]"),
            "{text}"
        );
        assert!(!text.contains("control"), "{text}");
        assert_eq!(structured.unwrap()["control"]["decision"], "allow_once");
    }

    #[test]
    fn a_denial_tells_the_model_not_to_retry() {
        let mut body = ErrorBody::new(
            ErrorCode::Forbidden,
            "the user denied `session.stop`: it was not run",
        );
        body.data = Some(json!({"control": {"decision": "deny"}}));
        let text = describe_remote(&body);
        assert!(text.starts_with("DENIED by the user"), "{text}");
        assert!(text.contains("Do not try again"), "{text}");
    }

    #[test]
    fn long_results_are_cut_on_a_character_boundary() {
        let text = "é".repeat(MAX_TEXT_BYTES);
        let cut = cut(text);
        assert!(cut.len() < MAX_TEXT_BYTES + 100);
        assert!(cut.ends_with("ask for less)"));
    }
}
