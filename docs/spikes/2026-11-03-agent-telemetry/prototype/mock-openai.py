import json, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer as HTTPServer
CALLS = []
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_GET(self):
        self.send_response(200); self.send_header("content-type","application/json"); self.end_headers()
        self.wfile.write(b'{"data":[]}')
    def do_POST(self):
        n = int(self.headers.get("content-length", 0)); body = json.loads(self.rfile.read(n) or b"{}")
        msgs = body.get("messages", [])
        has_tool_result = any(m.get("role") == "tool" for m in msgs)
        CALLS.append({"n_messages": len(msgs), "n_tools": len(body.get("tools", []) or [])})
        self.send_response(200); self.send_header("content-type","text/event-stream"); self.end_headers()
        def ev(o): self.wfile.write(("data: " + json.dumps(o) + "\n\n").encode()); self.wfile.flush()
        base = {"id":"c1","object":"chat.completion.chunk","created":1,"model":"mock-1"}
        if not has_tool_result:
            ev({**base,"choices":[{"index":0,"delta":{"role":"assistant","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read","arguments":""}}]}}]})
            ev({**base,"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\"hello.txt\"}"}}]}}]})
            ev({**base,"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
            usage = {"prompt_tokens": 11000, "completion_tokens": 40, "total_tokens": 11040, "prompt_tokens_details": {"cached_tokens": 4000}}
        else:
            ev({**base,"choices":[{"index":0,"delta":{"role":"assistant","content":"The file says hello."}}]})
            ev({**base,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            usage = {"prompt_tokens": 11200, "completion_tokens": 12, "total_tokens": 11212, "prompt_tokens_details": {"cached_tokens": 10000}}
        ev({**base,"choices":[],"usage":usage})
        self.wfile.write(b"data: [DONE]\n\n"); self.wfile.flush()
HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
