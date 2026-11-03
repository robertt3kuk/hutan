#!/usr/bin/env python3
"""Local mock of the Anthropic Messages API and the OpenAI Responses API (SSE), no network, no credentials.
Usage: mock_llm.py PORT LOGFILE
Scenario for a request with a plain user prompt: one tool call (Anthropic: Read of ./hello.txt; OpenAI: shell `echo hi`)
then, when the tool result comes back, a final text answer with fixed usage numbers.
"""
import json, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port = int(sys.argv[1]); logf = open(sys.argv[2], "a")


def log(kind, **kw):
    logf.write(json.dumps({"t": time.time(), "kind": kind, **kw}) + "\n"); logf.flush()


def sse(wfile, events):
    for ev, data in events:
        wfile.write(f"event: {ev}\ndata: {json.dumps(data)}\n\n".encode()); wfile.flush()


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *a): pass

    def do_HEAD(self): self.send_response(200); self.end_headers()

    def do_GET(self):
        log("GET", path=self.path)
        body = json.dumps({"data": [], "object": "list"}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n)
        try: req = json.loads(raw or b"{}")
        except Exception: req = {}
        path = self.path.split("?")[0]
        log("POST", path=path, model=req.get("model"), stream=req.get("stream"), bytes=n, tools=[t.get("name") for t in (req.get("tools") or [])][:40], nmsgs=len(req.get("messages", [])))
        if path.endswith("/v1/messages"):
            return self.anthropic(req)
        if path.endswith("/responses"):
            return self.openai_responses(req)
        if ":streamGenerateContent" in path or ":generateContent" in path:
            return self.gemini(req, ":stream" in path)
        if ":countTokens" in path:
            return self.json({"totalTokens": 100})
        if path.endswith("/count_tokens"):
            return self.json({"input_tokens": 100})
        self.json({"ok": True})

    def json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)

    # ---- Anthropic ----
    def anthropic(self, req):
        msgs = req.get("messages", [])
        last = msgs[-1] if msgs else {}
        has_result = isinstance(last.get("content"), list) and any(b.get("type") == "tool_result" for b in last["content"])
        has_read = any(t.get("name") == "Read" for t in req.get("tools", []) or [])
        main = req.get("model", "").startswith("claude-") and "haiku" not in req.get("model", "")
        anyres = any(isinstance(m.get('content'), list) and any(b.get('type') == 'tool_result' for b in m['content']) for m in msgs)
        tool_turn = (not has_result) and (not anyres) and has_read and main
        model = req.get("model", "claude-sonnet-5")
        if tool_turn:
            usage0 = {"input_tokens": 1200, "cache_creation_input_tokens": 3000, "cache_read_input_tokens": 0, "output_tokens": 1}
            content = {"type": "tool_use", "id": "toolu_mock01", "name": "Read", "input": {"file_path": req_cwd_file()}}
            stop, out = "tool_use", 30
            start_blk = {"type": "tool_use", "id": "toolu_mock01", "name": "Read", "input": {}}
            delta = {"type": "input_json_delta", "partial_json": json.dumps(content["input"])}
        else:
            usage0 = {"input_tokens": 1500, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 3000, "output_tokens": 1}
            stop, out = "end_turn", 12
            start_blk = {"type": "text", "text": ""}
            delta = {"type": "text_delta", "text": "Mock answer: hello from the local mock."}
        if not req.get("stream"):
            return self.json({"id": "msg_mock", "type": "message", "role": "assistant", "model": model,
                              "content": [{"type": "text", "text": "Mock title"}], "stop_reason": "end_turn",
                              "usage": {**usage0, "output_tokens": out}})
        self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("request-id", "req_mock%04d" % int(time.time() * 1000 % 10000)); self.end_headers()
        sse(self.wfile, [
            ("message_start", {"type": "message_start", "message": {"id": "msg_mock", "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": None, "stop_sequence": None, "usage": usage0}}),
            ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": start_blk}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": delta}),
            ("content_block_stop", {"type": "content_block_stop", "index": 0}),
            ("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": out}}),
            ("message_stop", {"type": "message_stop"}),
        ])

    # ---- Gemini ----
    def gemini(self, req, stream):
        contents = req.get("contents", [])
        has_resp = any("functionResponse" in p for c in contents for p in c.get("parts", []))
        tools = req.get("tools") or []
        names = [d.get("name") for t in tools for d in t.get("functionDeclarations", [])]
        log("gemini_req", tools=names[:30], ncontents=len(contents), sysinstr=bool(req.get("systemInstruction")))
        if (not has_resp) and "read_file" in names and len(contents) <= 2:
            parts = [{"functionCall": {"name": "read_file", "args": {"file_path": "hello.txt"}}}]
            usage = {"promptTokenCount": 3000, "candidatesTokenCount": 20, "totalTokenCount": 3020, "cachedContentTokenCount": 0}
            fin = "STOP"
        else:
            parts = [{"text": "Mock answer: hello from the local mock."}]
            usage = {"promptTokenCount": 3200, "candidatesTokenCount": 12, "totalTokenCount": 3212, "cachedContentTokenCount": 2000, "thoughtsTokenCount": 4}
            fin = "STOP"
        chunk = {"candidates": [{"content": {"role": "model", "parts": parts}, "finishReason": fin, "index": 0}], "usageMetadata": usage, "modelVersion": "gemini-mock"}
        if not stream:
            return self.json(chunk)
        self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
        self.wfile.write(("data: " + json.dumps(chunk) + "\r\n\r\n").encode()); self.wfile.flush()

    # ---- OpenAI Responses ----
    def openai_responses(self, req):
        inp = req.get("input", [])
        has_out = any(isinstance(i, dict) and i.get("type") in ("function_call_output", "local_shell_call_output") for i in inp)
        rid = "resp_mock%d" % int(time.time())
        try:
            open("logs/last-responses-req.json", "w").write(json.dumps(req)[:200000])
        except Exception: pass
        self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
        ev = [("response.created", {"type": "response.created", "response": {"id": rid}})]
        if not has_out and req.get("tools"):
            item = {"type": "function_call", "id": "fc_mock1", "call_id": "call_mock1", "name": "exec_command",
                    "arguments": json.dumps({"cmd": "echo hi", "yield_time_ms": 1000})}
            ev.append(("response.output_item.done", {"type": "response.output_item.done", "item": item}))
            usage = {"input_tokens": 2000, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 40, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2040}
        else:
            item = {"type": "message", "role": "assistant", "id": "msg_mock1", "content": [{"type": "output_text", "text": "Mock answer: hello from the local mock."}]}
            ev.append(("response.output_item.done", {"type": "response.output_item.done", "item": item}))
            usage = {"input_tokens": 2100, "input_tokens_details": {"cached_tokens": 1500}, "output_tokens": 15, "output_tokens_details": {"reasoning_tokens": 5}, "total_tokens": 2115}
        ev.append(("response.completed", {"type": "response.completed", "response": {"id": rid, "usage": usage}}))
        sse(self.wfile, ev)


def req_cwd_file():
    import os
    return os.path.join(os.environ.get("MOCK_CWD", os.getcwd()), "hello.txt")


ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
