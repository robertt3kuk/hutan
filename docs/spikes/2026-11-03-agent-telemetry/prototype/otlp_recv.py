#!/usr/bin/env python3
"""Minimal OTLP/HTTP receiver for the SPIKE-AGENT-TELEMETRY prototype.

Records every request to <outdir>/<n>-<signal>.json (decoded) and <n>.raw (body bytes, after gunzip).
Understands http/protobuf and http/json, gzip, any path; the first path segment(s) before /v1/<signal>
are recorded as the "prefix" (this is where a per-session token would live).
Usage: otlp_recv.py PORT OUTDIR [--require-prefix PREFIX]
"""
import gzip, json, os, sys, threading, time, zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from google.protobuf.json_format import MessageToDict, Parse
from opentelemetry.proto.collector.trace.v1 import trace_service_pb2 as ts
from opentelemetry.proto.collector.logs.v1 import logs_service_pb2 as ls
from opentelemetry.proto.collector.metrics.v1 import metrics_service_pb2 as ms

SIG = {"traces": (ts.ExportTraceServiceRequest, ts.ExportTraceServiceResponse),
       "logs": (ls.ExportLogsServiceRequest, ls.ExportLogsServiceResponse),
       "metrics": (ms.ExportMetricsServiceRequest, ms.ExportMetricsServiceResponse)}

port = int(sys.argv[1]); outdir = sys.argv[2]
require = None
if "--require-prefix" in sys.argv:
    require = sys.argv[sys.argv.index("--require-prefix") + 1]
strict = "--strict" in sys.argv          # design checks of SPIKE-AGENT-TELEMETRY §3: token in path, Host check, type check, size bounds
MAX_WIRE = 1 << 20; MAX_DECODED = 4 << 20
os.makedirs(outdir, exist_ok=True)
lock = threading.Lock(); counter = [0]


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def reply(self, code, body=b"", ctype="application/x-protobuf"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def reject(self, code, why):
        self.close_connection = True
        self.reply(code, b"")
        print(f"REJECT {code} {why}", flush=True)

    def do_POST(self):
        te = self.headers.get("Transfer-Encoding", "")
        n = int(self.headers.get("Content-Length") or 0)
        if strict:
            host = (self.headers.get("Host") or "").split(":")[0]
            if host not in ("127.0.0.1", "localhost"):
                return self.reject(421, "Host")                  # DNS rebinding
            if (self.headers.get("Content-Type") or "").split(";")[0] not in ("application/x-protobuf", "application/json"):
                return self.reject(415, "content-type")          # no text/plain "simple" cross-origin POST
            if n > MAX_WIRE:
                return self.reject(413, "wire size")
        if "chunked" in te.lower():
            raw = b""
            while True:
                size = int(self.rfile.readline().split(b";")[0].strip() or b"0", 16)
                if size == 0:
                    while self.rfile.readline().strip():  # trailers
                        pass
                    break
                raw += self.rfile.read(size); self.rfile.readline()
            n = len(raw)
        else:
            raw = self.rfile.read(n) if n else b""
        enc = (self.headers.get("Content-Encoding") or "").lower()
        if enc == "gzip":
            if strict:
                d = zlib.decompressobj(16 + zlib.MAX_WBITS)
                raw = d.decompress(raw, MAX_DECODED + 1)
                if len(raw) > MAX_DECODED:
                    return self.reject(413, "decoded size")      # gzip bomb
            else:
                raw = gzip.decompress(raw)
        elif enc == "deflate":
            raw = zlib.decompress(raw)
        path = self.path.split("?")[0]
        sig = next((s for s in SIG if path.endswith("/v1/" + s)), None)
        prefix = path[: -len("/v1/" + sig)] if sig else path
        with lock:
            counter[0] += 1
            i = counter[0]
        ctype = self.headers.get("Content-Type", "")
        hdrs = {k: ("<redacted>" if k.lower() == "authorization" else v) for k, v in self.headers.items()}
        if strict and not (sig and prefix == require):
            return self.reject(401, "token/path")
        if require is not None and not strict and prefix != require and self.headers.get("Authorization") != "Bearer " + require.strip("/").split("/")[-1]:
            self.reply(401, b"")
            print(f"#{i} 401 {self.path}", flush=True)
            return
        rec = {"n": i, "t": time.time(), "path": self.path, "prefix": prefix, "signal": sig,
               "content_type": ctype, "content_encoding": enc or None, "transfer_encoding": te or None,
               "wire_bytes": n, "decoded_bytes": len(raw), "headers": hdrs}
        if sig is None:
            rec["note"] = "unknown path"
            self.reply(404, b"")
        else:
            req_cls, resp_cls = SIG[sig]
            try:
                msg = req_cls()
                if "json" in ctype:
                    Parse(raw, msg)
                else:
                    msg.ParseFromString(raw)
                rec["body"] = MessageToDict(msg)
                body = resp_cls().SerializeToString() if "json" not in ctype else b"{}"
                self.reply(200, body, "application/json" if "json" in ctype else "application/x-protobuf")
            except Exception as e:  # keep the raw bytes for a post-mortem
                rec["decode_error"] = repr(e)
                self.reply(400, b"")
        with open(os.path.join(outdir, f"{i:04d}-{sig or 'x'}.json"), "w") as f:
            json.dump(rec, f, indent=1)
        with open(os.path.join(outdir, f"{i:04d}.raw"), "wb") as f:
            f.write(raw)
        print(f"#{i} {sig} {self.path} {ctype} enc={enc or '-'} {n}B->{len(raw)}B", flush=True)

    do_PUT = do_POST


ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
