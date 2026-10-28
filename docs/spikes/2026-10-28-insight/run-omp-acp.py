"""Drives `omp acp` against mock-openai.py and records the ACP wire (INSIGHT-SPIKE).

    SCRATCH=/tmp/insight-scratch python3 run-omp-acp.py

Everything lives under $SCRATCH: a fresh HOME / PI_CODING_AGENT_DIR (no login, no credentials),
a `models.yml` that points a `mock` provider at 127.0.0.1:18765, a `work` folder with one file.
No network beyond loopback; no model is called.
"""
import json, os, queue, subprocess, sys, threading, time
from pathlib import Path

here = Path(__file__).resolve().parent
scratch = Path(os.environ.get("SCRATCH", "/tmp/insight-scratch"))
agent_dir = scratch / "home" / "agent"
work = scratch / "work"
agent_dir.mkdir(parents=True, exist_ok=True)
work.mkdir(parents=True, exist_ok=True)
(work / "hello.txt").write_text("hello insight\n")
(agent_dir / "models.yml").write_text(
    """providers:
  mock:
    baseUrl: http://127.0.0.1:18765/v1
    apiKey: not-a-secret
    api: openai-completions
    models:
      - id: mock-1
        name: Mock 1
        reasoning: false
        input: [text]
        cost: { input: 3, output: 15, cacheRead: 0.3, cacheWrite: 3.75 }
        contextWindow: 200000
        maxTokens: 8192
"""
)
env = dict(os.environ, HOME=str(scratch / "home"), PI_CODING_AGENT_DIR=str(agent_dir),
           PROBE_OUT=str(scratch / "probe.out"))
(scratch / "probe.out").unlink(missing_ok=True)

server = subprocess.Popen([sys.executable, str(here / "mock-openai.py"), "18765"])
time.sleep(0.5)
p = subprocess.Popen(
    ["omp", "acp", "--model", "mock/mock-1", "-e", str(here / "omp-probe-extension.ts"),
     "--no-title", "--approval-mode", "yolo"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, cwd=work, env=env)
wire, lines = [], queue.Queue()
threading.Thread(target=lambda: ([lines.put(l) for l in p.stdout], lines.put(None)), daemon=True).start()

def send(o):
    wire.append({"dir": "out", "msg": o}); p.stdin.write(json.dumps(o) + "\n"); p.stdin.flush()

def read(until_id, t=40):
    end = time.time() + t
    while time.time() < end:
        try: l = lines.get(timeout=0.5)
        except queue.Empty: continue
        if l is None: return None
        m = json.loads(l); wire.append({"dir": "in", "msg": m})
        if m.get("id") == until_id and "method" not in m: return m

try:
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1, "clientCapabilities": {}}})
    read(1)
    send({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {"cwd": str(work), "mcpServers": []}})
    sid = read(2)["result"]["sessionId"]
    for n, text in ((3, "Read hello.txt and tell me what it says."), (4, "Say it again briefly."), (5, "/context")):
        send({"jsonrpc": "2.0", "id": n, "method": "session/prompt", "params": {"sessionId": sid, "prompt": [{"type": "text", "text": text}]}})
        print("prompt", n, json.dumps((read(n) or {}).get("result")))
finally:
    p.terminate(); server.terminate()
(scratch / "wire.json").write_text(json.dumps(wire))
print("wire:", scratch / "wire.json", "extension log:", scratch / "probe.out")
