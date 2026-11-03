#!/usr/bin/env python3
"""map_insight.py AGENT RECDIR : fold recorded OTLP requests into an InsightReport-shaped summary.

Prototype of the mapping a daemon receiver would do (SPIKE-AGENT-TELEMETRY). Output per agent:
  {"session": <agent's own session/conversation id>, "model": ..., "tokens": {input, output, cache_read,
   cache_write, reasoning} (input = uncached input, cache counted separately), "cost_usd": ..., "context": {used, window},
   "tools": [{"name", "calls", "failed", "ms"}], "turns": n, "dropped": [attribute names a receiver must never keep]}
Only counters/names/durations are read; prompt, tool arguments/outputs, file paths are never copied.
"""
import collections, glob, json, os, sys

def val(v):
    for k in ("stringValue", "intValue", "doubleValue", "boolValue"):
        if k in v: return v[k]
    if "arrayValue" in v: return [val(x) for x in v["arrayValue"].get("values", [])]
    return None

def attrs(l): return {a["key"]: val(a["value"]) for a in (l or [])}
def num(x):
    try: return float(x)
    except Exception: return None

class Acc:
    def __init__(s):
        s.session = None; s.models = collections.Counter(); s.tokens = collections.Counter(); s.cost = 0.0; s.has_cost = False
        s.ctx = None; s.tools = collections.defaultdict(lambda: {"calls": 0, "failed": 0, "ms": 0}); s.turns = 0; s.dropped = set()
    def tok(s, **kw):
        for k, v in kw.items():
            if v is not None: s.tokens[k] += int(v)
    def tool(s, name, ok, ms):
        t = s.tools[name]; t["calls"] += 1; t["failed"] += 0 if ok else 1; t["ms"] += int(ms or 0)
    def out(s):
        return {"session": s.session, "model": s.models.most_common(1)[0][0] if s.models else None, "tokens": dict(s.tokens),
                "cost_usd": round(s.cost, 6) if s.has_cost else None, "context": s.ctx,
                "tools": [{"name": k, **v} for k, v in sorted(s.tools.items())], "turns": s.turns, "dropped": sorted(s.dropped)}

def records(d):
    for f in sorted(glob.glob(os.path.join(d, "*-*.json"))):
        r = json.load(open(f)); b = r.get("body") or {}
        for rs in b.get("resourceLogs", []):
            for ss in rs.get("scopeLogs", []):
                for lr in ss.get("logRecords", []): yield "log", attrs(attrs_list(rs)), lr, attrs(lr.get("attributes")), val(lr.get("body", {})) if lr.get("body") else None
        for rs in b.get("resourceSpans", []):
            for ss in rs.get("scopeSpans", []):
                for sp in ss.get("spans", []): yield "span", attrs(attrs_list(rs)), sp, attrs(sp.get("attributes")), sp["name"]
        for rs in b.get("resourceMetrics", []):
            for ss in rs.get("scopeMetrics", []):
                for m in ss.get("metrics", []): yield "metric", attrs(attrs_list(rs)), m, {}, m["name"]

def attrs_list(rs): return rs.get("resource", {}).get("attributes")
def dur(a, k="duration_ms"): return num(a.get(k)) or 0

def claude(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind != "log": continue
        ev = a.get("event.name"); A.session = A.session or a.get("session.id")
        if ev == "api_request":
            A.models[a.get("model")] += 1
            A.tok(input=num(a.get("input_tokens")), output=num(a.get("output_tokens")), cache_read=num(a.get("cache_read_tokens")), cache_write=num(a.get("cache_creation_tokens")))
            c = num(a.get("cost_usd")); A.cost += c or 0; A.has_cost |= c is not None
            # Claude's input_tokens excludes cache read/write: context now = input + cache_read + cache_write of the last main request
            if a.get("query_source") in (None, "main", "repl_main_thread", "sdk"):
                A.ctx = {"used": int(sum(num(a.get(k)) or 0 for k in ("input_tokens", "cache_read_tokens", "cache_creation_tokens"))), "window": None, "note": "no window attribute in the OTLP stream"}
        elif ev == "tool_result": A.tool(a.get("tool_name"), a.get("success") == "true", dur(a))
        elif ev == "user_prompt": A.turns += 1
        for k in ("prompt", "prompt_text", "tool_input", "tool_parameters", "response", "error"):
            if k in a and a[k] not in ("<REDACTED>", None): A.dropped.add(k)
    return A.out()

def codex(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind != "log": continue
        ev = a.get("event.name"); A.session = A.session or a.get("conversation.id")
        if a.get("model"): A.models[a["model"]] += 1
        if ev == "codex.sse_event" and a.get("input_token_count") is not None:
            inp, cached = num(a.get("input_token_count")) or 0, num(a.get("cached_token_count")) or 0
            A.tok(input=inp - cached, output=num(a.get("output_token_count")), cache_read=cached, cache_write=num(a.get("cache_write_token_count")), reasoning=num(a.get("reasoning_token_count")))
            A.ctx = {"used": int(inp), "window": None, "note": "Codex logs carry no context window; it comes from the rollout's token_count or the model table"}
        elif ev == "codex.tool_result": A.tool(a.get("tool_name"), a.get("success") == "true", dur(a))
        elif ev == "codex.user_prompt": A.turns += 1
        for k in ("arguments", "output", "prompt"):
            if k in a and a[k] not in ("[REDACTED]", None): A.dropped.add(k)
    return A.out()

def omp(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind == "span" and name == "chat mock-1" or (kind == "span" and name.startswith("chat ")):
            A.session = A.session or a.get("gen_ai.conversation.id"); A.models[a.get("gen_ai.request.model")] += 1
            A.tok(input=num(a.get("gen_ai.usage.input_tokens")), output=num(a.get("gen_ai.usage.output_tokens")))
        elif kind == "span" and name.startswith("execute_tool"): A.tool(a.get("gen_ai.tool.name"), a.get("omp.gen_ai.tool.status") == "ok", 0)
        elif kind == "span" and name == "invoke_agent": A.turns += 1
        elif kind == "metric" and name == "omp.agent.chat.cost.estimated_usd":
            for rs in [raw]:
                for dp in rs["sum"]["dataPoints"]: A.cost += dp.get("asDouble", dp.get("asInt", 0)); A.has_cost = True
        elif kind == "metric" and name == "gen_ai.client.token.usage":
            for dp in raw["histogram"]["dataPoints"]:
                t = attrs(dp["attributes"]).get("gen_ai.token.type")
                if t == "cache_read_input": A.tokens["cache_read"] += int(dp["sum"])
    # OMP's gen_ai.usage.input_tokens includes cached reads: split them out
    if "cache_read" in A.tokens: A.tokens["input"] -= A.tokens["cache_read"]
    return A.out()

def gemini(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind != "log": continue
        ev = a.get("event.name"); A.session = A.session or a.get("session.id")
        if ev == "gemini_cli.api_response":
            A.models[a.get("model")] += 1; inp, c = num(a.get("input_token_count")) or 0, num(a.get("cached_content_token_count")) or 0
            A.tok(input=inp - c, output=num(a.get("output_token_count")), cache_read=c, reasoning=num(a.get("thoughts_token_count")))
        elif ev == "gemini_cli.tool_call": A.tool(a.get("function_name"), a.get("success") in (True, "true"), dur(a))
        elif ev == "gemini_cli.user_prompt": A.turns += 1
        for k in ("gen_ai.system_instructions", "gen_ai.input.messages", "gen_ai.output.messages", "function_args", "prompt"):
            if k in a: A.dropped.add(k)
    for kind, res, raw, a, name in records(d):
        if kind == "log": A.dropped.update(k for k in ("process.command_args",) if k in res)
        break
    return A.out()

def copilot(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind != "span": continue
        if name.startswith("chat "):
            A.session = A.session or a.get("gen_ai.conversation.id"); A.models[a.get("gen_ai.request.model")] += 1
            A.tok(input=num(a.get("gen_ai.usage.input_tokens")) - (num(a.get("gen_ai.usage.cache_read.input_tokens")) or 0), output=num(a.get("gen_ai.usage.output_tokens")), cache_read=num(a.get("gen_ai.usage.cache_read.input_tokens")), cache_write=num(a.get("gen_ai.usage.cache_creation.input_tokens")))
        elif name.startswith("execute_tool"): A.tool(a.get("gen_ai.tool.name"), "error.type" not in a, 0)
        elif name == "invoke_agent":
            A.turns += 1
            for ev in raw.get("events", []):
                if ev["name"] == "github.copilot.session.usage_info":
                    e = attrs(ev["attributes"]); A.ctx = {"used": int(num(e.get("github.copilot.current_tokens"))), "window": int(num(e.get("github.copilot.token_limit")))}
    return A.out()

def qwen(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind != "log": continue
        ev = a.get("event.name"); A.session = A.session or a.get("session.id")
        if ev == "qwen-code.api_response" and a.get("subagent_name") in (None, {}):
            A.models[a.get("model")] += 1; inp, c = num(a.get("input_token_count")) or 0, num(a.get("cached_content_token_count")) or 0
            A.tok(input=inp - c, output=num(a.get("output_token_count")), cache_read=c, reasoning=num(a.get("thoughts_token_count")))
        elif ev == "qwen-code.tool_call": A.tool(a.get("function_name"), a.get("success") in (True, "true"), dur(a)); A.dropped.add("function_args")
        elif ev == "qwen-code.user_prompt": A.turns += 1
    return A.out()

def goose(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind != "span": continue
        if name == "stream_response_from_provider":
            A.session = A.session or a.get("gen_ai.conversation.id"); A.models[a.get("gen_ai.request.model")] += 1
            A.tok(input=num(a.get("gen_ai.usage.input_tokens")) - (num(a.get("gen_ai.usage.cache_read.input_tokens")) or 0), output=num(a.get("gen_ai.usage.output_tokens")), cache_read=num(a.get("gen_ai.usage.cache_read.input_tokens")))
        elif name == "execute_tool": A.tool(a.get("gen_ai.tool.name"), "error.type" not in a, 0)
        elif name == "reply": A.turns += 1
    return A.out()

def opencode(d):
    A = Acc()
    for kind, res, raw, a, name in records(d):
        if kind == "span" and name == "ai.streamText":
            A.session = A.session or a.get("session.id"); A.models[a.get("ai.model.id")] += 1
            A.tok(input=num(a.get("ai.usage.inputTokenDetails.noCacheTokens")), output=num(a.get("ai.usage.outputTokens")), cache_read=num(a.get("ai.usage.inputTokenDetails.cacheReadTokens")), reasoning=num(a.get("ai.usage.reasoningTokens")))
            A.turns += 1
            for k in ("ai.prompt", "ai.response.text", "ai.response.toolCalls"): 
                if k in a: A.dropped.add(k)
    return A.out()

ADAPTERS = {"claude": claude, "codex": codex, "omp": omp, "gemini": gemini, "copilot": copilot, "qwen": qwen, "goose": goose, "opencode": opencode}
if __name__ == "__main__":
    print(json.dumps(ADAPTERS[sys.argv[1]](sys.argv[2]), indent=None))
