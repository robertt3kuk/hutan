#!/usr/bin/env python3
"""Sanitised, size-bounded copies of the recorded OTLP requests for docs/spikes/2026-11-03-agent-telemetry/fixtures/."""
import getpass, glob, json, os, re, socket, sys
S = os.path.expanduser("~/hutan-scratch/agent-telemetry"); OUT = sys.argv[1]
REPL = [(os.path.expanduser("~"), "/Users/me"), (socket.gethostname(), "host.local"), (getpass.getuser(), "me")]
DROPPED = {"ai.prompt", "ai.prompt.messages", "ai.response.text", "ai.response.toolCalls", "gen_ai.system_instructions", "ai.response.providerMetadata"}
def scrub(o):
    if isinstance(o, str):
        for a, b in REPL: o = o.replace(a, b)
        return o
    if isinstance(o, list): return [scrub(x) for x in o]
    if isinstance(o, dict): return {k: scrub(v) for k, v in o.items()}
    return o
def drop_attrs(o):
    if isinstance(o, dict):
        if "key" in o and "value" in o and o["key"] in DROPPED: return {"key": o["key"], "value": {"stringValue": "<dropped by the fixture script: content>"}}
        return {k: drop_attrs(v) for k, v in o.items()}
    if isinstance(o, list): return [drop_attrs(x) for x in o]
    return o
def load(path): return json.load(open(path))
def emit(agent, src, name, keep=None, spans=None, metrics=None):
    r = load(src); b = r["body"]
    if spans is not None:
        for rs in b.get("resourceSpans", []):
            for ss in rs["scopeSpans"]: ss["spans"] = [s for s in ss["spans"] if spans(s)]
    if metrics is not None:
        for rs in b.get("resourceMetrics", []):
            for ss in rs["scopeMetrics"]: ss["metrics"] = [m for m in ss["metrics"] if metrics(m)]
    if keep is not None:
        for rs in b.get("resourceLogs", []):
            for ss in rs["scopeLogs"]: ss["logRecords"] = [x for x in ss["logRecords"] if keep(x)]
    rec = {"agent": agent, "path": re.sub(r"/s/[A-Za-z0-9-]+", "/s/<token>", r["path"]), "content_type": r["content_type"], "transfer_encoding": r["transfer_encoding"],
           "wire_bytes_before_trim": r["wire_bytes"], "user_agent": r["headers"].get("User-Agent") or r["headers"].get("user-agent"), "body": drop_attrs(b)}
    d = os.path.join(OUT, agent); os.makedirs(d, exist_ok=True)
    json.dump(scrub(rec), open(os.path.join(d, name), "w"), separators=(",", ":"))
def ev(x, names):
    a = {k["key"]: (list(k["value"].values()) or [None])[0] for k in x.get("attributes", [])}
    return a.get("event.name") in names
for f, n in (("0001-traces", "traces"), ("0002-logs", "logs"), ("0003-metrics", "metrics")): emit("claude-code-2.1.316", f"{S}/rec/claude/{f}.json", f"{n}.json")
emit("codex-0.166.0", f"{S}/rec/codex-static/0002-logs.json", "logs-turn.json")
emit("codex-0.166.0", f"{S}/rec/codex-static/0001-logs.json", "logs-start.json")
keepm = {"codex.turn.token_usage", "codex.tool.call", "codex.tool.call.duration_ms", "codex.api_request", "codex.turn.e2e_duration_ms", "codex.conversation.turn.count", "codex.thread.started"}
emit("codex-0.166.0", f"{S}/rec/codex/0004.json".replace("0004.json", "0004-metrics.json"), "metrics-filtered.json", metrics=lambda m: m["name"] in keepm)
emit("codex-0.166.0", f"{S}/rec/codex/0003-traces.json", "traces-turn-filtered.json", spans=lambda s: s["name"] in ("run_turn", "codex.sampling", "build_tool_call", "responses.stream_request", "session_task.run"))
for f, n in (("0001-logs", "logs"), ("0002-traces", "traces"), ("0003-metrics", "metrics")): emit("omp-18.14.1", f"{S}/rec/omp/{f}.json", f"{n}.json")
emit("gemini-cli-0.66.0", f"{S}/rec/gemini/0001-logs.json", "logs-start.json")
emit("gemini-cli-0.66.0", f"{S}/rec/gemini/0003-metrics.json", "metrics-start.json")
emit("gemini-cli-0.66.0", f"{S}/rec/gemini/0004-logs.json", "logs-api.json", keep=lambda x: ev(x, {"gemini_cli.user_prompt", "gemini_cli.api_request", "gemini_cli.api_response", "gen_ai.client.inference.operation.details"}))
for f, n in (("0001-traces", "traces"), ("0002-metrics", "metrics")): emit("copilot-cli-1.0.105", f"{S}/rec/copilot/{f}.json", f"{n}.json")
emit("qwen-code-0.26.6", f"{S}/rec/qwen/0001-logs.json", "logs.json")
emit("qwen-code-0.26.6", f"{S}/rec/qwen/0004-metrics.json", "metrics.json", metrics=lambda m: m["name"].startswith("qwen-code.token") or m["name"].startswith("qwen-code.tool") or m["name"] == "qwen-code.session.count")
for f, n in (("0001-traces", "traces"), ("0002-metrics", "metrics"), ("0003-logs", "logs")): emit("goose-1.57.0", f"{S}/rec/goose/{f}.json", f"{n}.json")
emit("opencode-1.19.5", f"{S}/rec/opencode/0003-traces.json", "traces-ai-spans.json", spans=lambda s: s["name"].startswith("ai."))
emit("opencode-1.19.5", f"{S}/rec/opencode/0002-logs.json", "logs.json")
