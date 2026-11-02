import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer as HTTPServer

class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200); self.send_header("content-type","application/json"); self.end_headers()
        self.wfile.write(b'{"data":[]}')
    def do_POST(self):
        n = int(self.headers.get("content-length", 0)); body = json.loads(self.rfile.read(n) or b"{}")
        msgs = body.get("messages", [])
        stage = sum(1 for m in msgs if m.get("role") == "assistant")
        self.send_response(200); self.send_header("content-type","text/event-stream"); self.end_headers()
        def ev(o): self.wfile.write(("data: " + json.dumps(o) + "\n\n").encode()); self.wfile.flush()
        base = {"id":"c1","object":"chat.completion.chunk","created":1,"model":"mock-1"}
        last_user = [m for m in msgs if m.get("role") == "user"]
        text = json.dumps(last_user[-1].get("content")) if last_user else ""
        wants_tools = "TOOLS" in text and stage % 2 == 0 and not (msgs and msgs[-1].get("role") == "tool")
        after_tools = msgs and msgs[-1].get("role") == "tool"
        if wants_tools:
            calls = [("call_r", "read", {"path": "hello.txt"}), ("call_w", "write", {"path": "notes/out.txt", "content": "made by the mock\n"})]
            for i, (cid, name, args) in enumerate(calls):
                first = {"role":"assistant"} if i == 0 else {}
                ev({**base,"choices":[{"index":0,"delta":{**first,"tool_calls":[{"index":i,"id":cid,"type":"function","function":{"name":name,"arguments":""}}]}}]})
                ev({**base,"choices":[{"index":0,"delta":{"tool_calls":[{"index":i,"function":{"arguments":json.dumps(args)}}]}}]})
            ev({**base,"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
            usage = {"prompt_tokens": 11000, "completion_tokens": 60, "total_tokens": 11060, "prompt_tokens_details": {"cached_tokens": 4000}}
        else:
            ev({**base,"choices":[{"index":0,"delta":{"role":"assistant","content":"Done."}}]})
            ev({**base,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            usage = {"prompt_tokens": 11300, "completion_tokens": 8, "total_tokens": 11308, "prompt_tokens_details": {"cached_tokens": 10000}}
        ev({**base,"choices":[],"usage":usage})
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
