import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer as HTTPServer
LOG = sys.argv[2]
def text_of(m):
    c = m.get("content")
    if isinstance(c, str): return c
    if isinstance(c, list): return "".join(p.get("text", "") for p in c if isinstance(p, dict))
    return ""
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200); self.send_header("content-type","application/json"); self.end_headers(); self.wfile.write(b'{"data":[]}')
    def do_POST(self):
        n = int(self.headers.get("content-length", 0)); body = json.loads(self.rfile.read(n) or b"{}")
        msgs = body.get("messages", [])
        last_user = max((i for i, m in enumerate(msgs) if m.get("role") == "user"), default=-1)
        after = msgs[last_user + 1:] if last_user >= 0 else []
        has_result = any(m.get("role") == "tool" for m in after)
        utext = text_of(msgs[last_user]) if last_user >= 0 else ""
        system = "\n".join(text_of(m) for m in msgs if m.get("role") in ("system", "developer"))
        with open(LOG, "a") as f:
            f.write(json.dumps({"n_messages": len(msgs), "roles": [m.get("role") for m in msgs], "last_user": utext[:200], "system_len": len(system), "system": system}) + "\n")
        self.send_response(200); self.send_header("content-type","text/event-stream"); self.end_headers()
        def ev(o): self.wfile.write(("data: " + json.dumps(o) + "\n\n").encode()); self.wfile.flush()
        base = {"id":"c1","object":"chat.completion.chunk","created":1,"model":"mock-1"}
        if not has_result and utext.strip().lower().startswith("read "):
            path = utext.strip()[5:].strip().split()[0]
            ev({**base,"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]})
            ev({**base,"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":json.dumps({"path": path})}}]}}]})
            ev({**base,"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
            usage = {"prompt_tokens": 1000 + 100*len(msgs), "completion_tokens": 20, "total_tokens": 1020 + 100*len(msgs)}
        else:
            ev({**base,"choices":[{"index":0,"delta":{"role":"assistant","content":"ok: "+utext[:40]}}]})
            ev({**base,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            usage = {"prompt_tokens": 1000 + 100*len(msgs), "completion_tokens": 10, "total_tokens": 1010 + 100*len(msgs)}
        ev({**base,"choices":[],"usage":usage})
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
