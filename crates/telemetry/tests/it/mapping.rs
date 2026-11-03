//! Fixture round-trips: each agent's recording maps to the numbers of the spike's F11, in both
//! encodings; the allowlist keeps content out; retried requests count once.

use hutan_core::insight::{
    InsightOutcome, InsightReport, InsightSourceKind, ReportedTool, ReportedTurn, TurnPhase,
};
use hutan_telemetry::{Agent, Mapper, Signal};
use serde_json::{Value, json};

use crate::fixtures::{self, both, map};

const NOW: u64 = 1_800_000_000_000;

fn tools(reports: &[InsightReport]) -> Vec<ReportedTool> {
    reports.iter().flat_map(|r| r.tools.clone()).collect()
}

fn edges(reports: &[InsightReport]) -> Vec<(TurnPhase, Option<InsightOutcome>, Option<u64>)> {
    reports
        .iter()
        .filter_map(|r| r.turn)
        .map(
            |ReportedTurn {
                 phase,
                 outcome,
                 duration_ms,
             }| (phase, outcome, duration_ms),
        )
        .collect()
}

#[test]
fn claude_code_logs_map_to_tokens_cost_tools_and_a_turn_in_both_encodings() {
    let body = fixtures::load("claude-code-2.1.316", "logs.json");
    for decoded in both(Signal::Logs, &body) {
        let mapped = map(&decoded, NOW);
        assert_eq!(mapped.len(), 1);
        let m = &mapped[0];
        assert_eq!(m.agent, Agent::Claude);
        assert_eq!(m.service_version.as_deref(), Some("2.1.316"));
        assert_eq!(m.model.as_deref(), Some("claude-opus-5-5"));
        assert!(
            m.reports
                .iter()
                .all(|r| r.source == InsightSourceKind::Otlp)
        );
        // A turn begins with the prompt; what follows is one report.
        assert_eq!(edges(&m.reports), vec![(TurnPhase::Start, None, None)]);
        assert_eq!(m.reports.len(), 2);
        let content = &m.reports[1];
        let tokens = content.tokens.unwrap();
        // Two requests: input excludes the cache, as Anthropic counts (F11: 2 700 + 3 000 + 3 000 + 42).
        assert_eq!(tokens.input_tokens, Some(2_700));
        assert_eq!(tokens.cached_input_tokens, Some(3_000));
        assert_eq!(tokens.cache_write_tokens, Some(3_000));
        assert_eq!(tokens.output_tokens, Some(42));
        let cost = content.cost_usd.unwrap();
        assert!((cost - 0.02724).abs() < 1e-9, "{cost}");
        assert_eq!(content.api_ms, Some(22));
        assert_eq!(
            tools(&m.reports),
            vec![ReportedTool {
                name: "Read".into(),
                calls: 1,
                failed: 0,
                ms: Some(2)
            }]
        );
        // Nothing else: no context (Claude gives none), no files.
        assert!(content.context.is_none() && content.files.is_empty());
    }
}

#[test]
fn codex_logs_map_to_tokens_with_the_cache_inside_the_input_and_the_tool() {
    let body = fixtures::load("codex-0.166.0", "logs-turn.json");
    for decoded in both(Signal::Logs, &body) {
        let mapped = map(&decoded, NOW);
        assert_eq!(mapped.len(), 1);
        let m = &mapped[0];
        assert_eq!(m.agent, Agent::Codex);
        assert_eq!(m.model.as_deref(), Some("gpt-mock"));
        assert_eq!(edges(&m.reports), vec![(TurnPhase::Start, None, None)]);
        let tokens = m.reports[1].tokens.unwrap();
        // The counts as Codex gives them (`input_token_count` includes the cached ones).
        assert_eq!(tokens.input_tokens, Some(4_100));
        assert_eq!(tokens.cached_input_tokens, Some(1_500));
        assert_eq!(tokens.output_tokens, Some(55));
        assert_eq!(tokens.reasoning_output_tokens, Some(5));
        // Codex reports no cost.
        assert!(m.reports[1].cost_usd.is_none());
        assert_eq!(
            tools(&m.reports),
            vec![ReportedTool {
                name: "exec_command".into(),
                calls: 1,
                failed: 0,
                ms: Some(84)
            }]
        );
    }
}

#[test]
fn codex_start_up_events_map_to_nothing() {
    let body = fixtures::load("codex-0.166.0", "logs-start.json");
    for decoded in both(Signal::Logs, &body) {
        assert!(map(&decoded, NOW).is_empty());
    }
}

#[test]
fn genai_spans_of_copilot_goose_and_omp_map_to_the_same_numbers() {
    // The mock made the same two calls everywhere: 11 000 in (4 000 cached) + 40 out, then
    // 11 200 in (10 000 cached) + 12 out, and one `read` tool.
    for (dir, agent, failed) in [
        ("copilot-cli-1.0.105", Agent::Copilot, 1),
        // Goose's `read` carries `error.type = tool_not_available` in the recording.
        ("goose-1.57.0", Agent::Goose, 1),
        ("omp-18.14.1", Agent::Omp, 0),
    ] {
        let body = fixtures::load(dir, "traces.json");
        for decoded in both(Signal::Traces, &body) {
            let mapped = map(&decoded, NOW);
            assert_eq!(mapped.len(), 1, "{dir}");
            let m = &mapped[0];
            assert_eq!(m.agent, agent, "{dir}");
            assert_eq!(m.model.as_deref(), Some("mock-1"), "{dir}");
            let mut tokens = hutan_core::agent::TokenCounts::default();
            for report in &m.reports {
                if let Some(t) = report.tokens {
                    tokens.input_tokens =
                        Some(tokens.input_tokens.unwrap_or(0) + t.input_tokens.unwrap());
                    tokens.cached_input_tokens = Some(
                        tokens.cached_input_tokens.unwrap_or(0) + t.cached_input_tokens.unwrap(),
                    );
                    tokens.output_tokens =
                        Some(tokens.output_tokens.unwrap_or(0) + t.output_tokens.unwrap());
                }
            }
            // Input without the cache: (11 000 − 4 000) + (11 200 − 10 000). Goose's wrapping
            // `chat` span is not counted a second time.
            assert_eq!(tokens.input_tokens, Some(8_200), "{dir}");
            assert_eq!(tokens.cached_input_tokens, Some(14_000), "{dir}");
            assert_eq!(tokens.output_tokens, Some(52), "{dir}");
            let tools = tools(&m.reports);
            assert_eq!(tools.len(), 1, "{dir}");
            assert_eq!(tools[0].name, "read", "{dir}");
            assert_eq!(tools[0].calls, 1, "{dir}");
            assert_eq!(tools[0].failed, failed, "{dir}");
            // The `invoke_agent` span ends the turn.
            let ends = edges(&m.reports);
            assert_eq!(ends.len(), 1, "{dir}");
            assert_eq!(
                (ends[0].0, ends[0].1),
                (TurnPhase::End, Some(InsightOutcome::Completed))
            );
            assert!(ends[0].2.is_some());
        }
    }
}

#[test]
fn metrics_and_unmatched_shapes_map_to_nothing_never_to_zero() {
    for (dir, file) in [
        ("claude-code-2.1.316", "metrics.json"),
        ("codex-0.166.0", "metrics-filtered.json"),
        ("goose-1.57.0", "metrics.json"),
    ] {
        let body = fixtures::load(dir, file);
        for decoded in both(Signal::Metrics, &body) {
            assert!(map(&decoded, NOW).is_empty(), "{dir}/{file}");
        }
    }
    // Claude Code's trace spans are not GenAI spans; its logs carry the same facts.
    let body = fixtures::load("claude-code-2.1.316", "traces.json");
    for decoded in both(Signal::Traces, &body) {
        assert!(map(&decoded, NOW).is_empty());
    }
    // Gemini's logs and Qwen's are another agent's mapper (TEL-3): not read here.
    let body = fixtures::load("gemini-cli-0.66.0", "logs-api.json");
    for decoded in both(Signal::Logs, &body) {
        assert!(map(&decoded, NOW).is_empty());
    }
}

#[test]
fn a_request_is_counted_once_when_the_exporter_retries_it() {
    for (dir, file, signal) in [
        ("claude-code-2.1.316", "logs.json", Signal::Logs),
        ("codex-0.166.0", "logs-turn.json", Signal::Logs),
        ("goose-1.57.0", "traces.json", Signal::Traces),
    ] {
        let body = fixtures::load(dir, file);
        let decoded = fixtures::json(signal, &body);
        let mut mapper = Mapper::new();
        let first = mapper.map(&decoded, &|_| true, NOW);
        let again = mapper.map(&decoded, &|_| true, NOW + 1);
        assert!(!first.is_empty(), "{dir}");
        assert!(again.is_empty(), "{dir}: a retried request adds {again:?}");
    }
}

#[test]
fn a_cost_accumulates_across_requests() {
    let body = fixtures::load("claude-code-2.1.316", "logs.json");
    let decoded = fixtures::json(Signal::Logs, &body);
    let mut mapper = Mapper::new();
    mapper.map(&decoded, &|_| true, NOW);
    // The same two requests under other ids: a second conversation step of the same session.
    let mut again = body.clone();
    let text = again.to_string().replace("req_mock", "req_next").replace(
        "7ab1e759-59c3-4e61-a62e-b74acc499004",
        "00000000-0000-0000-0000-000000000001",
    );
    again = serde_json::from_str(&text).unwrap();
    let decoded = fixtures::json(Signal::Logs, &again);
    let mapped = mapper.map(&decoded, &|_| true, NOW + 10);
    let cost = mapped[0]
        .reports
        .iter()
        .rev()
        .find_map(|r| r.cost_usd)
        .unwrap();
    assert!((cost - 2.0 * 0.02724).abs() < 1e-9, "{cost}");
    // The turn that never ended is closed before the next one begins.
    let phases: Vec<_> = edges(&mapped[0].reports).iter().map(|e| e.0).collect();
    assert_eq!(phases, vec![TurnPhase::End, TurnPhase::Start]);
}

#[test]
fn a_turn_the_logs_never_end_is_closed_after_the_idle_time() {
    let body = fixtures::load("claude-code-2.1.316", "logs.json");
    let decoded = fixtures::json(Signal::Logs, &body);
    let mut mapper = Mapper::new();
    mapper.map(&decoded, &|_| true, NOW);
    assert_eq!(mapper.next_idle_at(30_000), Some(NOW + 30_000));
    assert!(mapper.close_idle(NOW + 29_999, 30_000).is_empty());
    let closed = mapper.close_idle(NOW + 30_000, 30_000);
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].agent, Agent::Claude);
    assert_eq!(
        edges(&closed[0].reports),
        vec![(TurnPhase::End, Some(InsightOutcome::Completed), None)]
    );
    // Once closed there is nothing left to close.
    assert_eq!(mapper.next_idle_at(30_000), None);
    assert!(mapper.close_idle(NOW + 99_999, 30_000).is_empty());
}

#[test]
fn an_agent_the_user_did_not_enable_is_not_read_at_all() {
    let body = fixtures::load("claude-code-2.1.316", "logs.json");
    let decoded = fixtures::json(Signal::Logs, &body);
    let mut mapper = Mapper::new();
    assert!(mapper.map(&decoded, &|a| a == Agent::Codex, NOW).is_empty());
    // Nothing was remembered: enabling it later counts the request.
    assert!(!mapper.map(&decoded, &|_| true, NOW).is_empty());
}

fn genai_span(extra: Vec<Value>) -> Value {
    let mut attrs = vec![
        json!({"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}}),
        json!({"key": "gen_ai.usage.input_tokens", "value": {"intValue": "100"}}),
        json!({"key": "gen_ai.usage.output_tokens", "value": {"intValue": "7"}}),
    ];
    attrs.extend(extra);
    json!({"resourceSpans": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "my-agent"}}]},
        "scopeSpans": [{"spans": [{
            "spanId": "00000000000000aa", "startTimeUnixNano": "1000000000", "endTimeUnixNano": "2000000000",
            "attributes": attrs
        }]}]
    }]})
}

#[test]
fn an_unknown_service_is_the_generic_agent_and_reads_the_hutan_extension() {
    let body = genai_span(vec![
        json!({"key": "hutan.cost.usd", "value": {"doubleValue": 1.25}}),
        json!({"key": "hutan.context.used_tokens", "value": {"intValue": "52000"}}),
        json!({"key": "hutan.context.window_tokens", "value": {"intValue": "200000"}}),
        json!({"key": "hutan.files.changed", "value": {"arrayValue": {"values": [
            {"stringValue": "update:src/main.rs"}, {"stringValue": "add:docs/a.md"},
            {"stringValue": "nonsense"}, {"stringValue": "delete:"}, {"intValue": "3"}
        ]}}}),
    ]);
    for decoded in both(Signal::Traces, &body) {
        let mapped = map(&decoded, NOW);
        let m = &mapped[0];
        assert_eq!(m.agent, Agent::Other);
        let r = &m.reports[0];
        assert_eq!(r.cost_usd, Some(1.25));
        let context = r.context.unwrap();
        assert_eq!(
            (context.used, context.window, context.estimated),
            (52_000, Some(200_000), false)
        );
        let files: Vec<_> = r.files.iter().map(|f| (f.path.as_str(), f.kind)).collect();
        use hutan_core::agent::FileChangeKind::{Add, Update};
        assert_eq!(files, vec![("src/main.rs", Update), ("docs/a.md", Add)]);
        // Without a statement the generic agent's input counts the cache (the conventions).
        assert_eq!(r.tokens.unwrap().input_tokens, Some(100));
        // And the whole report is valid for the reducer.
        r.clone().validated().unwrap();
    }
}

#[test]
fn the_resource_says_whether_the_input_includes_the_cache() {
    let mut body = genai_span(vec![json!({
        "key": "gen_ai.usage.cache_read.input_tokens", "value": {"intValue": "40"}
    })]);
    let tokens = |body: &Value| {
        map(&fixtures::json(Signal::Traces, body), NOW)[0].reports[0]
            .tokens
            .unwrap()
    };
    assert_eq!(tokens(&body).input_tokens, Some(60));
    body["resourceSpans"][0]["resource"]["attributes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"key": "hutan.usage.input_includes_cache", "value": {"boolValue": false}}));
    assert_eq!(tokens(&body).input_tokens, Some(100));
}

#[test]
fn content_never_reaches_a_report_a_status_or_a_debug_dump() {
    // The real recordings carry content (Codex's tool arguments and output, Claude's prompt
    // placeholders, Gemini's system instructions and command line, OpenCode's prompts).
    let mut canaries = vec!["echo hi", "Chunk ID", "REDACTED"];
    let mut bodies = Vec::new();
    for (dir, file, signal) in [
        ("codex-0.166.0", "logs-turn.json", Signal::Logs),
        ("claude-code-2.1.316", "logs.json", Signal::Logs),
        ("gemini-cli-0.66.0", "logs-start.json", Signal::Logs),
        ("opencode-1.19.5", "traces-ai-spans.json", Signal::Traces),
        ("qwen-code-0.26.6", "logs.json", Signal::Logs),
    ] {
        bodies.push((fixtures::load(dir, file), signal));
    }
    // And payloads built to leak: every content attribute of the spike's list.
    canaries.push("TOP-SECRET-7f3a");
    let leaking = |signal: Signal| {
        let attrs = [
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
        ]
        .map(|key| json!({"key": key, "value": {"stringValue": "TOP-SECRET-7f3a"}}));
        let mut body = if signal == Signal::Logs {
            json!({"resourceLogs": [{"resource": {"attributes": attrs.clone()}, "scopeLogs": [{"logRecords": [
                {"attributes": attrs.clone()}]}]}]})
        } else {
            json!({"resourceSpans": [{"resource": {"attributes": attrs.clone()}, "scopeSpans": [{"spans": [
                {"attributes": attrs.clone()}]}]}]})
        };
        // The value before its key, in JSON.
        let swapped = json!({"value": {"stringValue": "TOP-SECRET-7f3a"}, "key": "prompt"});
        let target = if signal == Signal::Logs {
            &mut body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["attributes"]
        } else {
            &mut body["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
        };
        target.as_array_mut().unwrap().push(swapped);
        body
    };
    bodies.push((leaking(Signal::Logs), Signal::Logs));
    bodies.push((leaking(Signal::Traces), Signal::Traces));

    for (body, signal) in &bodies {
        for decoded in both(*signal, body) {
            let dump = format!("{decoded:?}");
            let mut mapper = Mapper::new();
            let reports = format!("{:?}", mapper.map(&decoded, &|_| true, NOW));
            for canary in &canaries {
                assert!(
                    !dump.contains(canary),
                    "{canary} reached the decoded model: {dump}"
                );
                assert!(!reports.contains(canary), "{canary} reached a report");
            }
            for resource in &decoded.resources {
                let all = resource
                    .attrs
                    .iter()
                    .chain(resource.logs.iter().flat_map(|l| l.attrs.iter()))
                    .chain(resource.spans.iter().flat_map(|s| s.attrs.iter()));
                for (key, _) in all {
                    assert!(hutan_telemetry::allow::attribute(key), "{key} was kept");
                }
            }
        }
    }
}
