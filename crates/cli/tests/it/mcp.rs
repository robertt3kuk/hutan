//! `hutan mcp` over stdio against a test daemon: initialize, tools/list, a read, and a mutation
//! that goes through the approval path (allowed, denied, refused by policy).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common;

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use common::{HUTAN, TestDaemon, WAIT, command};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::{sleep, timeout};

/// A `hutan mcp` process and its JSON-RPC conversation.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: tokio::io::Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl Mcp {
    /// Started as an agent's host would: inside the session `origin`, if any.
    async fn start(socket: &Path, origin: Option<&str>) -> Self {
        let mut command = command(Path::new(HUTAN), Some(socket), &["mcp"]);
        match origin {
            Some(id) => command.env("HUTAN_SESSION_ID", id),
            None => command.env_remove("HUTAN_SESSION_ID"),
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    async fn send(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn read(&mut self) -> Value {
        let line = timeout(WAIT, self.stdout.next_line())
            .await
            .expect("no answer from hutan mcp")
            .unwrap()
            .expect("hutan mcp closed its output");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    /// Sends a request and returns its id.
    async fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    async fn answer(&mut self, id: u64) -> Value {
        let message = self.read().await;
        assert_eq!(message["id"], id, "{message}");
        message
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.request(method, params).await;
        self.answer(id).await
    }

    async fn initialize(&mut self) -> Value {
        let result = self
            .call(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"},
                }),
            )
            .await;
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        result
    }

    async fn tool_request(&mut self, name: &str, arguments: Value) -> u64 {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn text_of(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {response}"))
        .to_owned()
}

fn is_error(response: &Value) -> bool {
    response["result"]["isError"] == true
}

async fn start_with_policy(policy: &str) -> TestDaemon {
    let policy = policy.to_owned();
    TestDaemon::start_with(move |config| {
        std::fs::write(
            &config.paths.config_file,
            format!("[control]\nagents = \"{policy}\"\napproval_timeout_secs = 20\n"),
        )
        .unwrap();
    })
    .await
}

async fn report_agent(t: &TestDaemon, session: &str) {
    let mut report = command(
        Path::new(HUTAN),
        Some(&t.paths.socket),
        &[
            "agent", "report", "--source", "test", "--agent", "fake", "--state", "working",
            "--seq", "1",
        ],
    );
    report.env("HUTAN_SESSION_ID", session);
    common::finish(report).await.ok();
    timeout(WAIT, async {
        loop {
            if t.session(session).await.get("agent").is_some() {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the report never showed up");
}

async fn first_pending(t: &TestDaemon) -> Value {
    timeout(WAIT, async {
        loop {
            let out = t.hutan(&["approvals", "--json"]).await.ok();
            if let Some(first) = out.json()["approvals"].as_array().and_then(|a| a.first()) {
                return first.clone();
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("no approval was raised")
}

#[tokio::test(flavor = "multi_thread")]
async fn initialize_and_list_tools() {
    let t = TestDaemon::start().await;
    let mut mcp = Mcp::start(&t.paths.socket, None).await;
    let init = mcp.initialize().await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "hutan");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("approval")
    );

    // An older version the server also speaks is echoed; one it does not know gets its newest.
    let mut old = Mcp::start(&t.paths.socket, None).await;
    let result = old
        .call("initialize", json!({"protocolVersion": "2024-11-05"}))
        .await;
    assert_eq!(result["result"]["protocolVersion"], "2024-11-05");
    let result = old
        .call("initialize", json!({"protocolVersion": "1999-01-01"}))
        .await;
    assert_eq!(result["result"]["protocolVersion"], "2025-06-18");

    let list = mcp.call("tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "session_list",
        "session_create",
        "session_send_text",
        "session_output",
        "chat_create",
        "approval_list",
        "task_add",
        "worktree_new",
        "workspace_list",
        "notification_list",
        "history_search",
        "ui_actions",
        "ui_run",
        "ui_set_setting",
        "resources_list",
        "session_kill",
        "session_close",
        "session_reopen",
        "process_signal",
        "agent_interrupt",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    assert!(
        !names.iter().any(|n| n.contains("approval_respond")),
        "an agent cannot answer approvals: {names:?}"
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["description"].as_str().unwrap().len() > 40, "{tool}");
    }
    let rename = tools
        .iter()
        .find(|t| t["name"] == "session_rename")
        .unwrap();
    assert_eq!(rename["annotations"]["readOnlyHint"], false);
    let list_tool = tools.iter().find(|t| t["name"] == "session_list").unwrap();
    assert_eq!(list_tool["annotations"]["readOnlyHint"], true);

    // Ping, an unknown method, and a tool that does not exist.
    assert!(mcp.call("ping", json!({})).await["result"].is_object());
    let unknown = mcp.call("resources/list", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32601);
    let missing = mcp
        .call(
            "tools/call",
            json!({"name": "no_such_tool", "arguments": {}}),
        )
        .await;
    assert!(is_error(&missing), "{missing}");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_tool_returns_the_daemons_data() {
    let t = TestDaemon::start().await;
    let id = t
        .new_session(&[], &["sh", "-c", "echo hello-from-mcp; exec sleep 600"])
        .await;
    let mut mcp = Mcp::start(&t.paths.socket, None).await;
    mcp.initialize().await;

    let call = mcp.tool_request("session_list", json!({})).await;
    let list = mcp.answer(call).await;
    assert!(!is_error(&list), "{list}");
    assert!(text_of(&list).contains(&id), "{list}");
    let structured = &list["result"]["structuredContent"]["sessions"];
    assert!(
        structured
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == id.as_str())
    );

    // The output of a terminal, as lines.
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let call = mcp
            .tool_request("session_output", json!({"session_id": id}))
            .await;
        let output = mcp.answer(call).await;
        assert!(!is_error(&output), "{output}");
        if text_of(&output).contains("hello-from-mcp") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{output}");
        sleep(Duration::from_millis(100)).await;
    }

    // Bad arguments are the tool's failure, in words.
    let call = mcp.tool_request("session_output", json!({})).await;
    let bad = mcp.answer(call).await;
    assert!(is_error(&bad));
    assert!(text_of(&bad).contains("session_id"), "{bad}");
    let call = mcp
        .tool_request(
            "session_output",
            json!({"session_id": "01K7F3B5N2Q9W8E4R6T1Y0V3KM"}),
        )
        .await;
    let gone = mcp.answer(call).await;
    assert!(
        is_error(&gone) && text_of(&gone).contains("not_found"),
        "{gone}"
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mutation_in_an_agents_session_waits_for_the_users_approval() {
    let t = start_with_policy("ask").await;
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let target = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &agent).await;
    let mut mcp = Mcp::start(&t.paths.socket, Some(&agent)).await;
    mcp.initialize().await;

    // The call waits; the loop keeps answering meanwhile.
    let call = mcp
        .tool_request(
            "session_rename",
            json!({"session_id": target, "title": "via-mcp"}),
        )
        .await;
    let approval = first_pending(&t).await;
    assert_eq!(approval["kind"], "control");
    assert!(
        approval["title"].as_str().unwrap().contains("via-mcp"),
        "{approval}"
    );
    assert!(mcp.call("ping", json!({})).await["result"].is_object());
    t.hutan(&["approve", approval["id"].as_str().unwrap(), "allow_once"])
        .await
        .ok();
    let done = mcp.answer(call).await;
    assert!(!is_error(&done), "{done}");
    assert!(
        text_of(&done).contains("The user approved this request"),
        "{done}"
    );
    assert_eq!(
        done["result"]["structuredContent"]["control"]["decision"],
        "allow_once"
    );
    assert_eq!(t.session(&target).await["title"], "via-mcp");

    // Denied: an error result that tells the model not to try again.
    let call = mcp
        .tool_request(
            "session_rename",
            json!({"session_id": target, "title": "nope"}),
        )
        .await;
    let approval = first_pending(&t).await;
    t.hutan(&["approve", approval["id"].as_str().unwrap(), "deny"])
        .await
        .ok();
    let denied = mcp.answer(call).await;
    assert!(is_error(&denied), "{denied}");
    assert!(text_of(&denied).contains("DENIED"), "{denied}");
    assert!(text_of(&denied).contains("Do not try again"), "{denied}");
    assert_eq!(t.session(&target).await["title"], "via-mcp");

    // Reads of the same server are not gated.
    let call = mcp.tool_request("session_list", json!({})).await;
    assert!(!is_error(&mcp.answer(call).await));
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_policy_and_a_plain_shell_origin() {
    let t = start_with_policy("read_only").await;
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let shell = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let target = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &agent).await;

    let mut as_agent = Mcp::start(&t.paths.socket, Some(&agent)).await;
    as_agent.initialize().await;
    let call = as_agent
        .tool_request(
            "session_rename",
            json!({"session_id": target, "title": "agent"}),
        )
        .await;
    let refused = as_agent.answer(call).await;
    assert!(is_error(&refused), "{refused}");
    assert!(text_of(&refused).contains("REFUSED"), "{refused}");
    assert_ne!(t.session(&target).await["title"], "agent");

    // The same server started in a plain shell session (no agent reported there) is the user.
    let mut as_user = Mcp::start(&t.paths.socket, Some(&shell)).await;
    as_user.initialize().await;
    let call = as_user
        .tool_request(
            "session_rename",
            json!({"session_id": target, "title": "user"}),
        )
        .await;
    let done = as_user.answer(call).await;
    assert!(!is_error(&done), "{done}");
    assert_eq!(t.session(&target).await["title"], "user");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_and_reopening_a_session_are_mutations_that_wait_for_approval() {
    let t = start_with_policy("ask").await;
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let target = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &agent).await;
    let mut mcp = Mcp::start(&t.paths.socket, Some(&agent)).await;
    mcp.initialize().await;

    let call = mcp
        .tool_request("session_close", json!({"session_id": target}))
        .await;
    let approval = first_pending(&t).await;
    assert_eq!(approval["kind"], "control");
    assert!(
        approval["title"].as_str().unwrap().contains("close"),
        "{approval}"
    );
    t.hutan(&["approve", approval["id"].as_str().unwrap(), "allow_once"])
        .await
        .ok();
    let done = mcp.answer(call).await;
    assert!(!is_error(&done), "{done}");
    assert!(
        t.sessions()
            .await
            .iter()
            .all(|s| s["id"] != target.as_str()),
        "closed"
    );

    let call = mcp
        .tool_request("session_reopen", json!({"session_id": target}))
        .await;
    let approval = first_pending(&t).await;
    assert!(
        approval["title"].as_str().unwrap().contains("reopen"),
        "{approval}"
    );
    t.hutan(&["approve", approval["id"].as_str().unwrap(), "allow_once"])
        .await
        .ok();
    let done = mcp.answer(call).await;
    assert!(!is_error(&done), "{done}");
    t.wait_state(&target, "running").await;
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_policy_refuses_to_close_sessions() {
    let t = start_with_policy("read_only").await;
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let target = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &agent).await;
    let mut mcp = Mcp::start(&t.paths.socket, Some(&agent)).await;
    mcp.initialize().await;
    let call = mcp
        .tool_request(
            "session_close",
            json!({"session_id": target, "stop_running": true}),
        )
        .await;
    let refused = mcp.answer(call).await;
    assert!(is_error(&refused), "{refused}");
    assert!(text_of(&refused).contains("REFUSED"), "{refused}");
    assert_eq!(t.session(&target).await["state"]["state"], "running");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_tools_read_freely_and_only_propose_nothing_is_saved_until_the_user_approves() {
    // Even under `agents = "allow"`: a proposal changes nothing and waits for the user.
    let t = start_with_policy("allow").await;
    t.hutan(&["memory", "add", "Prefers short answers."])
        .await
        .ok();
    let agent = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &agent).await;
    let mut mcp = Mcp::start(&t.paths.socket, Some(&agent)).await;
    let init = mcp.initialize().await;
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("never say it was saved"),
        "{init}"
    );
    let list = mcp.call("tools/list", json!({})).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for wanted in ["memory_list", "memory_search", "memory_propose"] {
        assert!(names.contains(&wanted), "{wanted} missing from {names:?}");
    }
    assert!(
        !names.iter().any(|n| matches!(
            *n,
            "memory_write" | "memory_update" | "memory_archive" | "memory_restore"
        )),
        "no direct write tool for an agent: {names:?}"
    );

    // Reads are free and carry the revision a proposal needs.
    let call = mcp.tool_request("memory_list", json!({})).await;
    let listed = mcp.answer(call).await;
    assert!(!is_error(&listed), "{listed}");
    let entries = listed["result"]["structuredContent"]["entries"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(entries.len(), 1);
    let call = mcp
        .tool_request("memory_search", json!({"query": "SHORT"}))
        .await;
    let found = mcp.answer(call).await;
    assert_eq!(
        found["result"]["structuredContent"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let call = mcp
        .tool_request("memory_search", json!({"query": "nothing like this"}))
        .await;
    assert!(
        mcp.answer(call).await["result"]["structuredContent"]["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // A proposal comes back `proposed`, and the text says it is not saved.
    let call = mcp
        .tool_request(
            "memory_propose",
            json!({"op": "create", "scope": "global", "category": "fact", "body": "Staging is called kite."}),
        )
        .await;
    let proposed = mcp.answer(call).await;
    assert!(!is_error(&proposed), "{proposed}");
    let text = text_of(&proposed);
    assert!(
        text.contains("NOT saved to memory until the user approves"),
        "{text}"
    );
    assert_eq!(
        proposed["result"]["structuredContent"]["status"],
        "proposed"
    );
    assert_eq!(proposed["result"]["structuredContent"]["applied"], false);
    let listing = t.hutan(&["memory", "ls", "--json"]).await.ok().json();
    assert_eq!(
        listing["entries"].as_array().unwrap().len(),
        1,
        "nothing saved yet"
    );
    let approvals = t.hutan(&["approvals", "--json"]).await.ok().json();
    assert_eq!(
        approvals["approvals"].as_array().unwrap().len(),
        1,
        "{approvals}"
    );

    // A bad proposal is refused in words, and a credential is not remembered.
    let call = mcp
        .tool_request(
            "memory_propose",
            json!({"op": "create", "scope": "global", "category": "fact", "body": "password = hunter2hunter2"}),
        )
        .await;
    let refused = mcp.answer(call).await;
    assert!(is_error(&refused), "{refused}");
    assert!(
        text_of(&refused).contains("Memory is not a secret store"),
        "{refused}"
    );

    // The user approves from the CLI; only then is it an entry, and the model's own entry is
    // still labelled inferred.
    let id = approvals["approvals"][0]["id"].as_str().unwrap().to_owned();
    t.hutan(&["approve", &id, "approve"]).await.ok();
    let listing = t.hutan(&["memory", "ls", "--json"]).await.ok().json();
    let entries = listing["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .any(|e| e["body"] == "Staging is called kite." && e["inferred"] == true)
    );
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn git_history_tools_read_without_asking_and_never_write() {
    let t = TestDaemon::start().await;
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap().join("repo");
    std::fs::create_dir(&root).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Ada")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Ada")
            .env("GIT_COMMITTER_EMAIL", "ada@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(root.join("f.txt"), "one\ntwo\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "first"]);
    std::fs::write(root.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git(&["commit", "-q", "-am", "second"]);
    t.hutan(&["project", "open", root.to_str().unwrap()])
        .await
        .ok();

    let mut mcp = Mcp::start(&t.paths.socket, None).await;
    let list = mcp.initialize().await;
    let _ = list;
    let tools = mcp.call("tools/list", json!({})).await;
    for name in ["git_log", "git_blame"] {
        let tool = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not listed"));
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{name}");
    }

    let call = mcp
        .tool_request(
            "git_log",
            json!({"path": root.display().to_string(), "limit": 1}),
        )
        .await;
    let log = mcp.answer(call).await;
    assert!(!is_error(&log), "{log}");
    let commits = &log["result"]["structuredContent"]["commits"];
    assert_eq!(commits[0]["subject"], "second", "{log}");
    assert_eq!(log["result"]["structuredContent"]["more"], true);

    // `limit` is optional for an agent.
    let call = mcp
        .tool_request(
            "git_log",
            json!({"path": root.join("f.txt").display().to_string()}),
        )
        .await;
    let file_log = mcp.answer(call).await;
    assert_eq!(
        file_log["result"]["structuredContent"]["commits"]
            .as_array()
            .unwrap()
            .len(),
        2,
        "{file_log}"
    );

    let call = mcp
        .tool_request(
            "git_blame",
            json!({"path": root.join("f.txt").display().to_string()}),
        )
        .await;
    let blame = mcp.answer(call).await;
    assert!(!is_error(&blame), "{blame}");
    let hunks = blame["result"]["structuredContent"]["hunks"]
        .as_array()
        .unwrap();
    assert_eq!(hunks.len(), 2, "{blame}");
    assert_eq!(
        blame["result"]["structuredContent"]["commits"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    t.stop().await;
}

/// SANDBOX-1: an agent can read a chat's safety state (it is part of the session's `chat`), but
/// the `chat_create` tool does not offer to choose one: widening what a new agent may do is the
/// user's decision, taken in the Start sheet or on the command line.
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_a_chats_safety_state_but_the_chat_create_tool_cannot_set_one() {
    let work = tempfile::tempdir().unwrap();
    let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11/plain-turn.jsonl");
    let toml = format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", {transcript:?}, \"acp\"]\n",
        bin = common::acp_fixture_bin(),
    );
    let t = TestDaemon::start_toml(&toml, |_| {}).await;
    let cwd = work.path().display().to_string();
    let created = t
        .hutan(&[
            "chat",
            "new",
            "--agent",
            "omp",
            "--cwd",
            &cwd,
            "--safety",
            "read_only",
            "--json",
        ])
        .await
        .ok()
        .json();
    let id = created["session"]["id"].as_str().unwrap().to_owned();

    let mut mcp = Mcp::start(&t.paths.socket, None).await;
    mcp.initialize().await;
    let call = mcp.tool_request("session_list", json!({})).await;
    let list = mcp.answer(call).await;
    assert!(!is_error(&list), "{list}");
    let session = list["result"]["structuredContent"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id.as_str())
        .expect("the chat is listed")
        .clone();
    assert_eq!(session["chat"]["safety"]["preset"], "read_only");
    assert_eq!(session["chat"]["safety"]["enforced_by"], "harness");

    let tools = mcp.call("tools/list", json!({})).await;
    let create = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "chat_create")
        .unwrap()
        .clone();
    assert!(
        create["inputSchema"]["properties"].get("safety").is_none(),
        "{create}"
    );
    t.stop().await;
}

/// Agent insight (protocol 0.59): `agent_insight` reads what a chat's context holds and its turns
/// spent, without sending anything to the agent; it is read-only, and nothing in the table can
/// ask the agent for `/context` (that is the user's button).
#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_a_chats_insight_and_no_tool_measures_its_context() {
    let work = tempfile::tempdir().unwrap();
    let transcript = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agents/tests/fixtures/acp-omp-18.11/insight-omp.jsonl");
    let toml = format!(
        "[terminal]\nenv_inherit = \"daemon\"\n\n[harness.omp]\nbin = {bin:?}\nargs = [\"replay\", {transcript:?}, \"acp\"]\n",
        bin = common::acp_fixture_bin(),
    );
    let t = TestDaemon::start_toml(&toml, |_| {}).await;
    let cwd = work.path().display().to_string();
    let created = t
        .hutan(&["chat", "new", "--agent", "omp", "--cwd", &cwd, "--json"])
        .await
        .ok()
        .json();
    let id = created["session"]["id"].as_str().unwrap().to_owned();
    for text in [
        "Read hello.txt and tell me what it says.",
        "Say it again briefly.",
    ] {
        t.hutan(&["chat", "send", &id, text, "--wait"]).await.ok();
    }

    // Inside the chat's own session: an agent asking about itself, under the default policy.
    let mut mcp = Mcp::start(&t.paths.socket, Some(&id)).await;
    mcp.initialize().await;
    let call = mcp
        .tool_request("agent_insight", json!({"session_id": id}))
        .await;
    let answer = mcp.answer(call).await;
    assert!(!is_error(&answer), "{answer}");
    let insight = &answer["result"]["structuredContent"]["insight"];
    assert_eq!(insight["harness"], "omp");
    assert_eq!(insight["context"]["used"], 11_200);
    assert_eq!(insight["totals"]["turns"], 2);
    assert_eq!(insight["totals"]["input"], 9_400);
    assert_eq!(insight["unsupported"], json!(["reasoning"]));

    let tools = mcp.call("tools/list", json!({})).await;
    let names: Vec<_> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.iter().any(|n| n == "agent_insight"));
    assert!(
        !names.iter().any(|n| n.contains("measure")),
        "measuring is the user's button: {names:?}"
    );
    t.stop().await;
}

/// The board tools (protocol 0.63): read-only, and what they return is text others wrote, so the
/// result opens with a notice saying it is data and `_meta` says it is untrusted (R7). The
/// tools that answer nothing about the user's own words do not carry it.
#[tokio::test(flavor = "multi_thread")]
async fn the_board_tools_read_and_mark_their_output_as_untrusted_data() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &id).await;

    let mut mcp = Mcp::start(&t.paths.socket, None).await;
    mcp.initialize().await;

    let listed = mcp.call("tools/list", json!({})).await;
    for name in ["agents_board", "question_list", "history_sessions"] {
        let tool = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{name}");
    }

    let call = mcp.tool_request("agents_board", json!({})).await;
    let answer = mcp.answer(call).await;
    assert!(!is_error(&answer), "{answer}");
    let text = text_of(&answer);
    assert!(text.starts_with("UNTRUSTED DATA"), "{text}");
    assert_eq!(answer["result"]["_meta"]["hutan/untrusted"], true);
    let board = &answer["result"]["structuredContent"];
    assert_eq!(board["agents"][0]["session_id"], id);
    assert_eq!(board["agents"][0]["source"], "hook");
    assert_eq!(board["coverage"]["terminal_reported"], 1);

    let call = mcp.tool_request("question_list", json!({})).await;
    let answer = mcp.answer(call).await;
    assert!(!is_error(&answer), "{answer}");
    assert!(text_of(&answer).starts_with("UNTRUSTED DATA"));
    assert_eq!(
        answer["result"]["structuredContent"]["questions"],
        json!([])
    );

    // The history index is off in a fresh home: the tool says so, as an error, not as data.
    let call = mcp
        .tool_request("history_sessions", json!({"limit": 500}))
        .await;
    let answer = mcp.answer(call).await;
    assert!(is_error(&answer), "{answer}");
    assert!(!text_of(&answer).contains("UNTRUSTED DATA"), "{answer}");

    // Other tools are unchanged.
    let call = mcp.tool_request("approval_list", json!({})).await;
    let answer = mcp.answer(call).await;
    assert!(!is_error(&answer), "{answer}");
    assert!(answer["result"].get("_meta").is_none());
    t.stop().await;
}

/// `agents_activity` (protocol 0.65): read-only, takes a window in minutes, says what is not
/// recorded, and its result is marked untrusted like the board's.
#[tokio::test(flavor = "multi_thread")]
async fn agents_activity_reads_a_window_and_is_marked_untrusted() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    report_agent(&t, &id).await;

    let mut mcp = Mcp::start(&t.paths.socket, None).await;
    mcp.initialize().await;
    let listed = mcp.call("tools/list", json!({})).await;
    let tool = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "agents_activity")
        .expect("the tool");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);

    let call = mcp
        .tool_request("agents_activity", json!({"last_minutes": 60}))
        .await;
    let answer = mcp.answer(call).await;
    assert!(!is_error(&answer), "{answer}");
    assert!(text_of(&answer).starts_with("UNTRUSTED DATA"));
    assert_eq!(answer["result"]["_meta"]["hutan/untrusted"], true);
    let activity = &answer["result"]["structuredContent"];
    assert_eq!(activity["summary"]["sessions_started"], 1);
    assert!(
        activity["to_ms"].as_u64().unwrap() - activity["from_ms"].as_u64().unwrap()
            <= 3_600_000 + 5_000,
        "{activity}"
    );
    assert!(!activity["not_recorded"].as_array().unwrap().is_empty());

    // A window that is not a number of minutes is a bad argument.
    let call = mcp
        .tool_request("agents_activity", json!({"last_minutes": "an hour"}))
        .await;
    assert!(is_error(&mcp.answer(call).await));
    t.stop().await;
}

/// `hutan mcp --read-only-tools` is how the daemon learns which tools a Mawas harness may run
/// without asking (MAWAS-READS): it prints the table's reads, needs no daemon and starts no
/// server.
#[tokio::test(flavor = "multi_thread")]
async fn the_read_only_tools_are_listed_without_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(HUTAN)
        .args(["mcp", "--read-only-tools"])
        .env("HUTAN_HOME", home.path())
        .env_remove("HUTAN_SOCKET")
        .env_remove("HUTAN_SESSION_ID")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    let names: Vec<&str> = text.lines().collect();
    for read in [
        "agents_board",
        "agents_activity",
        "question_list",
        "history_sessions",
    ] {
        assert!(names.contains(&read), "{read} in {names:?}");
    }
    assert!(!names.contains(&"chat_send") && !names.contains(&"chat_queue_add"));
    assert!(
        std::fs::read_dir(home.path()).unwrap().next().is_none(),
        "it never started a daemon"
    );
}
