//! The public "Report your agent's stats to Hutan" contract, level L2 (`docs/integrations.md`):
//! `hutan agent report --insight-json`. The JSON Schema checked in at
//! `docs/schemas/agent-insight-report-v1.json` says what a sender may put in it; these tests
//! keep the schema, the Rust type that reads the report, the examples and the documented limits
//! from drifting apart, and run a third-party agent (`integrations/examples/third-party-agent.sh`)
//! and every example through the real `hutan` and a real daemon into the insight record.
//!
//! There is no JSON Schema crate in the workspace; [`validate`] understands exactly the keywords
//! the schema uses, and [`the_validator_understands_every_keyword_of_the_schema`] fails when the
//! schema starts using one it does not, so a keyword can never be silently ignored.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use crate::common::{self, TestDaemon, WAIT};

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use hutan_core::agent::{FileChangeKind, TokenCounts};
use hutan_core::insight::{
    InsightOutcome, InsightReport, InsightSourceKind, MAX_PATH_BYTES, MAX_REPORT_BYTES,
    MAX_REPORT_FILES, MAX_REPORT_TOOLS, ReportedContext, ReportedFile, ReportedTool, ReportedTurn,
    TurnPhase,
};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

// The schema and its examples -----------------------------------------------------------------

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema() -> Value {
    let text = include_str!("../../../../docs/schemas/agent-insight-report-v1.json");
    serde_json::from_str(text).expect("the schema is JSON")
}

fn examples_dir() -> PathBuf {
    repo().join("integrations/examples/insight-reports")
}

/// `(file stem, document)` of every example report, in name order.
fn examples() -> Vec<(String, Value)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(examples_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let doc = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: not JSON: {e}", path.display()));
            (
                path.file_stem().unwrap().to_string_lossy().into_owned(),
                doc,
            )
        })
        .collect()
}

// A validator for the keywords the schema uses ------------------------------------------------

/// Keywords that only describe.
const ANNOTATIONS: &[&str] = &["$schema", "title", "description", "default", "examples"];
/// Keywords [`validate`] enforces.
const ENFORCED: &[&str] = &[
    "type",
    "properties",
    "required",
    "enum",
    "minimum",
    "minLength",
    "maxLength",
    "maxItems",
    "items",
    "additionalProperties",
];

/// Every schema node (not the property names under `properties`).
fn schema_nodes<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(node);
    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        for sub in properties.values() {
            schema_nodes(sub, out);
        }
    }
    if let Some(items) = node.get("items") {
        schema_nodes(items, out);
    }
}

#[test]
fn the_validator_understands_every_keyword_of_the_schema() {
    let schema = schema();
    let mut nodes = Vec::new();
    schema_nodes(&schema, &mut nodes);
    for node in nodes {
        for (keyword, value) in node.as_object().expect("a schema node is an object") {
            assert!(
                ANNOTATIONS.contains(&keyword.as_str()) || ENFORCED.contains(&keyword.as_str()),
                "the schema uses `{keyword}`, which validate() does not enforce"
            );
            if keyword == "additionalProperties" {
                // Unknown fields are ignored by Hutan, so the schema must keep allowing them.
                assert_eq!(value, &json!(true));
            }
        }
    }
}

/// Checks `doc` against `schema`; every violation is pushed as `path: what`.
fn validate(schema: &Value, doc: &Value, at: &str, errors: &mut Vec<String>) {
    if let Some(allowed) = schema.get("enum") {
        if !allowed.as_array().unwrap().contains(doc) {
            errors.push(format!("{at}: {doc} is not one of {allowed}"));
        }
        return;
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let ok = match kind {
            "object" => doc.is_object(),
            "array" => doc.is_array(),
            "string" => doc.is_string(),
            "boolean" => doc.is_boolean(),
            "integer" => doc.is_i64() || doc.is_u64(),
            "number" => doc.is_number(),
            other => panic!("unknown type {other}"),
        };
        if !ok {
            errors.push(format!("{at}: {doc} is not a {kind}"));
            return;
        }
    }
    match doc {
        Value::Object(map) => {
            for name in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = name.as_str().unwrap();
                if !map.contains_key(name) {
                    errors.push(format!("{at}: `{name}` is required"));
                }
            }
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, sub) in properties {
                    if let Some(value) = map.get(name) {
                        validate(sub, value, &format!("{at}.{name}"), errors);
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
                && items.len() as u64 > max
            {
                errors.push(format!("{at}: {} items, at most {max}", items.len()));
            }
            if let Some(sub) = schema.get("items") {
                for (n, item) in items.iter().enumerate() {
                    validate(sub, item, &format!("{at}[{n}]"), errors);
                }
            }
        }
        Value::String(text) => {
            let chars = text.chars().count() as u64;
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                && chars < min
            {
                errors.push(format!("{at}: shorter than {min}"));
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
                && chars > max
            {
                errors.push(format!("{at}: longer than {max}"));
            }
        }
        Value::Number(number) => {
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
                && number.as_f64().unwrap() < min
            {
                errors.push(format!("{at}: {number} is below {min}"));
            }
        }
        _ => {}
    }
}

fn violations(doc: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    validate(&schema(), doc, "$", &mut errors);
    errors
}

#[test]
fn the_validator_catches_what_it_should() {
    // A validator that accepts everything would make every other test here vacuous.
    for (doc, mentions) in [
        (json!({"tools": "read"}), "$.tools"),
        (json!({"context": {}}), "`used` is required"),
        (json!({"cost_usd": -0.5}), "below"),
        (json!({"files": [{"path": "a", "kind": "rename"}]}), "kind"),
        (json!({"tokens": {"input_tokens": 1.5}}), "not a integer"),
        (json!({"source": "chat"}), "$.source"),
    ] {
        let errors = violations(&doc);
        assert!(
            errors.iter().any(|e| e.contains(mentions)),
            "{doc}: expected a violation mentioning `{mentions}`, got {errors:?}"
        );
    }
    assert_eq!(violations(&json!({})), Vec::<String>::new());
}

// The schema against the type that reads the report -----------------------------------------

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

fn property_names(node: &Value) -> Vec<String> {
    let mut names: Vec<String> = node["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    names.sort();
    names
}

/// A report with every field set, so serialising it lists every field the type has.
fn full_report() -> InsightReport {
    InsightReport {
        source: InsightSourceKind::Hooks,
        turn: Some(ReportedTurn {
            phase: TurnPhase::End,
            outcome: Some(InsightOutcome::Completed),
            duration_ms: Some(1),
        }),
        tokens: Some(TokenCounts {
            input_tokens: Some(1),
            cached_input_tokens: Some(1),
            cache_write_tokens: Some(1),
            output_tokens: Some(1),
            reasoning_output_tokens: Some(1),
            total_tokens: Some(1),
        }),
        context: Some(ReportedContext {
            used: 1,
            window: Some(2),
            fixed: Some(1),
            estimated: true,
        }),
        cost_usd: Some(0.5),
        api_ms: Some(1),
        tools: vec![ReportedTool {
            name: "read".into(),
            calls: 2,
            failed: 1,
            ms: Some(1),
        }],
        files: vec![ReportedFile {
            path: "a.rs".into(),
            kind: FileChangeKind::Add,
        }],
    }
}

#[test]
fn the_schema_lists_exactly_the_fields_the_report_type_has() {
    let schema = schema();
    let report = serde_json::to_value(full_report()).unwrap();
    assert_eq!(keys(&report), property_names(&schema), "top level");
    for part in ["turn", "tokens", "context"] {
        assert_eq!(
            keys(&report[part]),
            property_names(&schema["properties"][part]),
            "{part}"
        );
    }
    for part in ["tools", "files"] {
        assert_eq!(
            keys(&report[part][0]),
            property_names(&schema["properties"][part]["items"]),
            "{part}[]"
        );
    }
    // What the schema demands is what the type cannot do without.
    let required = |node: &Value| -> Vec<String> {
        let mut names: Vec<String> = node["required"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .map(|n| n.as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    };
    assert_eq!(required(&schema), Vec::<String>::new());
    assert_eq!(required(&schema["properties"]["turn"]), ["phase"]);
    assert_eq!(required(&schema["properties"]["context"]), ["used"]);
    assert_eq!(required(&schema["properties"]["tools"]["items"]), ["name"]);
    assert_eq!(
        required(&schema["properties"]["files"]["items"]),
        ["kind", "path"]
    );
}

fn enum_of(node: &Value) -> Vec<String> {
    let mut names: Vec<String> = node["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

/// The wire names of `values`, sorted.
fn wire_names<T: serde::Serialize>(values: &[T]) -> Vec<String> {
    let mut names: Vec<String> = values
        .iter()
        .map(|v| {
            serde_json::to_value(v)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn the_schemas_enums_are_the_types_variants() {
    use InsightSourceKind::{Chat, Extension, Hooks, Osc7501, Statusline, Transcript};
    // The matches stop compiling when a variant is added: add it here and to the schema.
    let sources = [Chat, Extension, Hooks, Statusline, Transcript, Osc7501];
    for source in sources {
        match source {
            Chat | Extension | Hooks | Statusline | Transcript | Osc7501 => {}
        }
    }
    let outcomes = [
        InsightOutcome::Running,
        InsightOutcome::Completed,
        InsightOutcome::Failed,
        InsightOutcome::Cancelled,
    ];
    for outcome in outcomes {
        match outcome {
            InsightOutcome::Running
            | InsightOutcome::Completed
            | InsightOutcome::Failed
            | InsightOutcome::Cancelled => {}
        }
    }
    let phases = [TurnPhase::Start, TurnPhase::End];
    for phase in phases {
        match phase {
            TurnPhase::Start | TurnPhase::End => {}
        }
    }
    let kinds = [
        FileChangeKind::Add,
        FileChangeKind::Update,
        FileChangeKind::Delete,
    ];
    for kind in kinds {
        match kind {
            FileChangeKind::Add | FileChangeKind::Update | FileChangeKind::Delete => {}
        }
    }
    let schema = schema();
    // `chat` is the daemon's own and is refused, so the schema leaves it out.
    let mut senders = wire_names(&sources);
    senders.retain(|name| name != "chat");
    assert_eq!(enum_of(&schema["properties"]["source"]), senders);
    assert_eq!(
        enum_of(&schema["properties"]["turn"]["properties"]["outcome"]),
        wire_names(&outcomes)
    );
    assert_eq!(
        enum_of(&schema["properties"]["turn"]["properties"]["phase"]),
        wire_names(&phases)
    );
    assert_eq!(
        enum_of(&schema["properties"]["files"]["items"]["properties"]["kind"]),
        wire_names(&kinds)
    );
}

#[test]
fn the_schemas_limits_are_the_daemons() {
    let schema = schema();
    let tools = &schema["properties"]["tools"];
    let files = &schema["properties"]["files"];
    assert_eq!(tools["maxItems"], MAX_REPORT_TOOLS);
    assert_eq!(files["maxItems"], MAX_REPORT_FILES);
    assert_eq!(
        files["items"]["properties"]["path"]["maxLength"],
        MAX_PATH_BYTES
    );
    assert_eq!(MAX_REPORT_BYTES, 4096, "the documented size");
    assert!(
        schema["description"]
            .as_str()
            .unwrap()
            .contains("4096 bytes"),
        "the schema says how large a report may be"
    );
    // The tool-name limit is private to the type; the daemon cuts at 64 bytes.
    assert_eq!(tools["items"]["properties"]["name"]["maxLength"], 64);
    let cut = InsightReport {
        tools: vec![ReportedTool {
            name: "n".repeat(100),
            calls: 1,
            failed: 0,
            ms: None,
        }],
        ..InsightReport::default()
    }
    .validated()
    .unwrap();
    assert_eq!(cut.tools[0].name.len(), 64);
}

// The examples ---------------------------------------------------------------------------------

#[test]
fn every_example_is_valid_and_is_a_report_the_daemon_keeps_whole() {
    let all = examples();
    assert!(all.len() >= 5, "the examples are there: {all:?}");
    for (name, doc) in all {
        let errors = violations(&doc);
        assert!(errors.is_empty(), "{name}: {errors:?}");
        let report: InsightReport = serde_json::from_value(doc.clone())
            .unwrap_or_else(|e| panic!("{name}: the daemon cannot read it: {e}"));
        assert!(!report.is_empty(), "{name} says something");
        let kept = report
            .clone()
            .validated()
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(kept, report, "{name}: nothing was cut");
        assert!(serde_json::to_vec(&doc).unwrap().len() <= MAX_REPORT_BYTES);
    }
}

#[test]
fn fields_a_newer_sender_adds_stay_valid() {
    let doc = json!({
        "tools": [{"name": "read", "future": 1}],
        "context": {"used": 5, "future": [1]},
        "future": {"a": 1}
    });
    assert!(violations(&doc).is_empty());
    let report: InsightReport = serde_json::from_value(doc).unwrap();
    assert_eq!(report.tools.len(), 1);
}

/// How the daemon treats a document.
#[derive(Debug, PartialEq)]
enum Daemon {
    /// The report does not parse: the CLI drops it and sends nothing.
    Unreadable,
    /// It parses and the daemon answers an error.
    Refuses,
    /// It is taken, with the offending part cut or dropped.
    Cuts,
}

fn daemon_outcome(doc: &Value) -> Daemon {
    match serde_json::from_value::<InsightReport>(doc.clone()) {
        Err(_) => Daemon::Unreadable,
        Ok(report) => match report.clone().validated() {
            Err(_) => Daemon::Refuses,
            Ok(kept) if kept != report => Daemon::Cuts,
            Ok(_) => panic!("{doc}: the daemon keeps it whole"),
        },
    }
}

#[test]
fn what_the_schema_refuses_the_daemon_refuses_or_cuts() {
    let many_tools: Vec<Value> = (0..=MAX_REPORT_TOOLS)
        .map(|n| json!({"name": format!("t{n}")}))
        .collect();
    // (document, does the schema refuse it, what the daemon does). Two rules a schema without
    // conditionals cannot hold are documented, not enforced: a turn that ends is not `running`,
    // and a path has no control character.
    let cases = [
        (json!({"source": "chat"}), true, Daemon::Refuses),
        (
            json!({"turn": {"phase": "end", "outcome": "running"}}),
            false,
            Daemon::Refuses,
        ),
        (
            json!({"turn": {"phase": "pause"}}),
            true,
            Daemon::Unreadable,
        ),
        (
            json!({"tokens": {"input_tokens": -1}}),
            true,
            Daemon::Unreadable,
        ),
        (
            json!({"context": {"window": 100}}),
            true,
            Daemon::Unreadable,
        ),
        (json!({"tools": [{"calls": 2}]}), true, Daemon::Unreadable),
        (
            json!({"files": [{"path": "a", "kind": "rename"}]}),
            true,
            Daemon::Unreadable,
        ),
        (json!({"cost_usd": -1}), true, Daemon::Cuts),
        (json!({"tools": many_tools}), true, Daemon::Cuts),
        (
            json!({"tools": [{"name": "x".repeat(65)}]}),
            true,
            Daemon::Cuts,
        ),
        (
            json!({"files": [{"path": "a\nb", "kind": "add"}]}),
            false,
            Daemon::Cuts,
        ),
    ];
    for (doc, schema_refuses, daemon) in cases {
        assert_eq!(
            !violations(&doc).is_empty(),
            schema_refuses,
            "{doc}: {:?}",
            violations(&doc)
        );
        assert_eq!(daemon_outcome(&doc), daemon, "{doc}");
    }
}

// Through the real CLI and a real daemon ----------------------------------------------------

/// `hutan agent report …` as an agent inside the session `id` runs it.
fn agent_report(socket: &Path, id: &str, args: &[&str]) -> Command {
    let mut command = common::command(Path::new(common::HUTAN), Some(socket), &["agent", "report"]);
    command
        .args(args)
        .env("HUTAN_SESSION_ID", id)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

async fn report(t: &TestDaemon, id: &str, args: &[&str]) {
    let out = common::finish(agent_report(&t.paths.socket, id, args)).await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", ""),
        "a report never fails or speaks"
    );
}

/// The `--insight-json -` form: the document on standard input.
async fn report_stdin(t: &TestDaemon, id: &str, args: &[&str], stdin: &str) {
    let mut command = agent_report(&t.paths.socket, id, args);
    command.stdin(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut pipe = child.stdin.take().unwrap();
    pipe.write_all(stdin.as_bytes()).await.unwrap();
    drop(pipe);
    let out = timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
}

/// `hutan chat insight ID --json` once `ready` holds for it.
async fn insight_when(t: &TestDaemon, id: &str, ready: impl Fn(&Value) -> bool) -> Value {
    timeout(WAIT, async {
        loop {
            let out = t.hutan(&["chat", "insight", id, "--json"]).await;
            if out.code == 0 {
                let insight = out.json();
                if ready(&insight) {
                    return insight;
                }
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the insight arrived")
}

fn tool_names(insight: &Value) -> Vec<String> {
    let mut names: Vec<String> = insight["tools"]["by_name"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| row["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn source_of(insight: &Value, block: &str) -> Option<String> {
    insight["sources"]
        .as_array()?
        .iter()
        .find(|s| s["block"] == block)
        .map(|s| s["source"].as_str().unwrap().to_owned())
}

fn example_script() -> PathBuf {
    repo().join("integrations/examples/third-party-agent.sh")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_third_party_example_gives_an_unknown_agent_an_insight() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    // As a process of the session runs it: `HUTAN_SESSION_ID` and `HUTAN_SOCKET` set, `hutan`
    // named by `HUTAN`.
    let mut command = Command::new("sh");
    command
        .arg(example_script())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HUTAN", common::HUTAN)
        .env("HUTAN_SESSION_ID", &id)
        .env("HUTAN_SOCKET", &t.paths.socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = common::finish(command).await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );

    // The last report (the end of the turn) comes last: wait for it.
    let insight = insight_when(&t, &id, |i| i["turns"][0]["outcome"] == "completed").await;
    assert_eq!(
        insight["harness"], "acme",
        "the agent is named as it reported"
    );
    let totals = &insight["totals"];
    assert_eq!(totals["turns"], 1);
    assert_eq!(totals["input"], 2400, "tokens of two calls add up");
    assert_eq!(totals["output"], 335);
    assert_eq!(totals["cache_read"], 19_900);
    assert_eq!(
        totals["cost_usd"], 0.0733,
        "the cost is a level, not a delta"
    );
    assert!(
        totals.get("cache_hit_ratio").is_none_or(Value::is_null),
        "no cache ratio for an agent Hutan does not know: {totals}"
    );
    assert!(
        totals.get("reasoning").is_none_or(Value::is_null),
        "a counter nobody reported is absent, never zero: {totals}"
    );
    assert_eq!(insight["context"]["used"], 11_600, "the latest level wins");
    assert_eq!(insight["context"]["window"], 200_000);
    assert_eq!(insight["time"]["api_ms"], 5200);
    assert_eq!(insight["tools"]["calls"], 4);
    assert_eq!(insight["tools"]["failed"], 1);
    assert_eq!(tool_names(&insight), ["bash", "edit", "read"]);
    let edit = insight["tools"]["by_name"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["name"] == "edit")
        .unwrap();
    assert_eq!(
        (edit["calls"].as_u64(), edit["failed"].as_u64()),
        (Some(1), Some(1))
    );
    let files: Vec<(&str, &str)> = insight["files"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["path"].as_str().unwrap(), f["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(files.len(), 2);
    assert!(
        files.contains(&("src/main.rs", "update")) && files.contains(&("notes/todo.md", "add"))
    );
    let turn = &insight["turns"][0];
    assert_eq!(turn["outcome"], "completed");
    assert_eq!(turn["duration_ms"], 4200);
    assert_eq!(turn["tool_calls"], 4);
    // Each block says where it came from: the default source of a report is `extension`.
    for block in ["context", "turns", "tools", "files", "time"] {
        assert_eq!(
            source_of(&insight, block).as_deref(),
            Some("extension"),
            "{block}"
        );
    }
    // The agent is in the agent tree too: idle again after the turn.
    let agent = t.session(&id).await["agent"].clone();
    assert_eq!(agent["agent"], "acme");
    assert_eq!(agent["source"], "acme-agent");
    assert_eq!(agent["state"], "idle");
}

/// The script's own reports, caught instead of sent: each is valid against the schema, so what
/// the example teaches is what the schema says.
#[tokio::test(flavor = "multi_thread")]
async fn the_third_party_example_sends_only_reports_the_schema_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("reports.jsonl");
    let fake = dir.path().join("hutan");
    std::fs::write(
        &fake,
        "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = --insight-json ]; then cat; printf '\\n'; fi\ndone >> \"$HUTAN_RECORD\"\necho \"$*\" >> \"$HUTAN_RECORD.args\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let status = Command::new("sh")
        .arg(example_script())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HUTAN", &fake)
        .env("HUTAN_RECORD", &record)
        .status()
        .await
        .unwrap();
    assert!(status.success());
    let sent: Vec<Value> = std::fs::read_to_string(&record)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(sent.len(), 5, "five insights: {sent:?}");
    for doc in &sent {
        assert!(violations(doc).is_empty(), "{doc}: {:?}", violations(doc));
    }
    let examples: Vec<Value> = examples().into_iter().map(|(_, doc)| doc).collect();
    assert_eq!(
        sent, examples,
        "the script sends the example files, in order"
    );
    let args = std::fs::read_to_string(dir.path().join("reports.jsonl.args")).unwrap();
    assert!(
        args.lines()
            .all(|l| l.starts_with("agent report --source acme-agent --agent acme")),
        "{args}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inline_json_and_standard_input_give_the_same_insight() {
    let t = TestDaemon::start().await;
    let inline = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let piped = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    for id in [&inline, &piped] {
        report(
            &t,
            id,
            &["--source", "s", "--agent", "acme", "--state", "idle"],
        )
        .await;
    }
    for (_, doc) in examples() {
        let text = doc.to_string();
        report(
            &t,
            &inline,
            &["--source", "s", "--agent", "acme", "--insight-json", &text],
        )
        .await;
        report_stdin(
            &t,
            &piped,
            &["--source", "s", "--agent", "acme", "--insight-json", "-"],
            &text,
        )
        .await;
    }
    let done = |i: &Value| i["turns"][0]["outcome"] == "completed";
    let (a, b) = (
        insight_when(&t, &inline, done).await,
        insight_when(&t, &piped, done).await,
    );
    for block in ["totals", "context", "tools", "files"] {
        assert_eq!(
            without_clock(&a[block]),
            without_clock(&b[block]),
            "{block}"
        );
        assert!(!a[block].is_null(), "{block} is there");
    }
    assert_eq!(a["time"]["api_ms"], b["time"]["api_ms"]);
    assert_eq!(a["time"]["api_ms"], 5200);
}

/// `value` without the timestamps (`as_of_ms`), which differ between two sessions.
fn without_clock(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| key.as_str() != "as_of_ms")
                .map(|(key, value)| (key.clone(), without_clock(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_clock).collect()),
        other => other.clone(),
    }
}

/// What a sender gets wrong is dropped silently, never breaks the agent, and never reaches the
/// record; what is right still arrives.
#[tokio::test(flavor = "multi_thread")]
async fn bad_reports_change_nothing_and_never_fail() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let base = ["--source", "s", "--agent", "acme"];
    let with = |text: &str| {
        let mut args: Vec<String> = base.iter().map(|s| (*s).to_owned()).collect();
        args.extend(["--insight-json".to_owned(), text.to_owned()]);
        args
    };
    report(&t, &id, &[base.as_slice(), &["--state", "idle"]].concat()).await;
    let oversize = json!({"tools": [{"name": "huge-report"}], "files": [{"path": "p".repeat(5000), "kind": "add"}]}).to_string();
    let bad = [
        "{not json".to_owned(),
        r#"{"tools":[{"name":"daemons-own"}],"source":"chat"}"#.to_owned(),
        r#"{"tools":[{"name":"backwards"}],"turn":{"phase":"end","outcome":"running"}}"#.to_owned(),
        r#"{"tools":[{"name":"negative"}],"tokens":{"input_tokens":-5}}"#.to_owned(),
        oversize,
    ];
    for text in &bad {
        let args = with(text);
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        report(&t, &id, &refs).await;
    }
    // Part cut, rest kept: a negative cost goes, 16 of 20 tool rows stay.
    let many: Vec<Value> = (0..20)
        .map(|n| json!({"name": format!("row{n}")}))
        .collect();
    let cut = json!({"cost_usd": -3.0, "tools": many}).to_string();
    let args = with(&cut);
    report(
        &t,
        &id,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
    .await;
    // An unknown field is ignored; the marker (a block with at most one value) shows that the
    // stream was processed up to here.
    let args = with(r#"{"api_ms":777,"from_the_future":true}"#);
    report(
        &t,
        &id,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )
    .await;

    let insight = insight_when(&t, &id, |i| i["time"]["api_ms"] == 777).await;
    let names = tool_names(&insight);
    for dropped in ["huge-report", "daemons-own", "backwards", "negative"] {
        assert!(
            !names.contains(&dropped.to_owned()),
            "{dropped} got in: {names:?}"
        );
    }
    assert_eq!(insight["tools"]["calls"], 16, "16 of the 20 rows");
    assert!(!names.contains(&"row16".to_owned()));
    assert!(
        insight["totals"].is_null() || insight["totals"]["cost_usd"].is_null(),
        "the negative cost is dropped: {}",
        insight["totals"]
    );
}

/// Outside a Hutan session (no `HUTAN_SESSION_ID`) a report is a no-op, so an agent can call it
/// unconditionally.
#[tokio::test(flavor = "multi_thread")]
async fn outside_a_hutan_session_a_report_is_a_quiet_no_op() {
    let t = TestDaemon::start().await;
    let mut command = common::command(
        Path::new(common::HUTAN),
        Some(&t.paths.socket),
        &[
            "agent",
            "report",
            "--source",
            "s",
            "--agent",
            "acme",
            "--insight-json",
            r#"{"tools":[{"name":"x"}]}"#,
        ],
    );
    command.env_remove("HUTAN_SESSION_ID");
    let out = common::finish(command).await;
    assert_eq!(
        (out.code, out.stdout.as_str(), out.stderr.as_str()),
        (0, "", "")
    );
}

/// The agent exists for Hutan from its first state report. Insight sent before it is kept and
/// shows from then on, but nothing can be read before: that is why the contract says a report
/// of the state comes first.
#[tokio::test(flavor = "multi_thread")]
async fn insight_sent_before_the_first_state_report_shows_once_the_agent_exists() {
    let t = TestDaemon::start().await;
    let id = t.new_session(&[], &["sh", "-c", "exec sleep 600"]).await;
    let base = ["--source", "s", "--agent", "acme"];
    report(
        &t,
        &id,
        &[
            base.as_slice(),
            &[
                "--insight-json",
                r#"{"tools":[{"name":"early"}],"api_ms":5}"#,
            ],
        ]
        .concat(),
    )
    .await;
    let before = t.hutan(&["chat", "insight", &id, "--json"]).await;
    assert_ne!(before.code, 0, "no agent yet: {before:?}");
    assert!(
        before
            .stderr
            .contains("neither a chat nor a terminal that runs an agent")
    );
    report(&t, &id, &[base.as_slice(), &["--state", "idle"]].concat()).await;
    let insight = insight_when(&t, &id, |i| tool_names(i) == ["early"]).await;
    assert_eq!(insight["harness"], "acme");
    assert_eq!(insight["time"]["api_ms"], 5);
}
