"""Drives `omp acp` (18.12.3) against mock.py: session/fork into another cwd, /add-dir, /dirs, additionalDirectories (BEST-OF spike).
    SCRATCH=<dir> python3 run.py
Scratch HOME / PI_CODING_AGENT_DIR (no login), loopback mock provider, no model call."""
import json, os, queue, subprocess, sys, threading, time, glob
from pathlib import Path
here = Path(__file__).resolve().parent
scratch = Path(os.environ.get("SCRATCH", str(here / "s")))
for d in ("home/agent", "workA", "workB", "workC"): (scratch / d).mkdir(parents=True, exist_ok=True)
(scratch/"workA/a.txt").write_text("alpha\n"); (scratch/"workB/b.txt").write_text("bravo\n"); (scratch/"workC/c.txt").write_text("charlie\n")
agent_dir = scratch / "home/agent"
(agent_dir / "models.yml").write_text("""providers:
  mock:
    baseUrl: http://127.0.0.1:18766/v1
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
""")
log = scratch / "requests.jsonl"; log.unlink(missing_ok=True)
env = dict(os.environ, HOME=str(scratch/"home"), PI_CODING_AGENT_DIR=str(agent_dir))
server = subprocess.Popen([sys.executable, str(here/"mock.py"), "18766", str(log)]); time.sleep(0.5)
p = subprocess.Popen(["omp","acp","--model","mock/mock-1","--no-title","--approval-mode","yolo"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, cwd=scratch/"workA", env=env)
lines = queue.Queue(); threading.Thread(target=lambda: ([lines.put(l) for l in p.stdout], lines.put(None)), daemon=True).start()
wire = []; ctr = [10]
def send(m, params):
    ctr[0] += 1; i = ctr[0]; o = {"jsonrpc":"2.0","id":i,"method":m,"params":params}; wire.append({"dir":"out","msg":o}); p.stdin.write(json.dumps(o)+"\n"); p.stdin.flush(); return i
def call(m, params, t=40):
    i = send(m, params); texts = []; end = time.time()+t
    while time.time() < end:
        try: l = lines.get(timeout=0.5)
        except queue.Empty: continue
        if l is None: return {"error":"eof"}, texts
        d = json.loads(l); wire.append({"dir":"in","msg":d})
        u = (d.get("params") or {}).get("update") or {}
        if u.get("sessionUpdate") == "agent_message_chunk": texts.append(u["content"].get("text",""))
        if d.get("id") == i and "method" not in d: return d, "".join(texts)
    return {"error":"timeout"}, "".join(texts)
def prompt(sid, text): r, t = call("session/prompt", {"sessionId":sid,"prompt":[{"type":"text","text":text}]}); return r.get("result") or r.get("error"), t
out = {}
try:
    call("initialize", {"protocolVersion":1,"clientCapabilities":{}})
    r, _ = call("session/new", {"cwd": str(scratch/"workA"), "mcpServers": [], "additionalDirectories": [str(scratch/"workB")]})
    out["new_with_additionalDirectories"] = r.get("result", r.get("error")) and (list((r.get("result") or {}).keys()) or r.get("error"))
    sid = r["result"]["sessionId"]
    out["dirs_after_new"] = prompt(sid, "/dirs")[1]
    out["turn1"] = prompt(sid, "read a.txt")[1]
    out["turn2"] = prompt(sid, "say hi")[1]
    out["add_dir"] = prompt(sid, f"/add-dir {scratch/'workB'}")[1]
    out["dirs_after_add"] = prompt(sid, "/dirs")[1]
    out["read_extra"] = prompt(sid, f"read {scratch/'workB'/'b.txt'}")[1]
    # fork straight into another cwd: refused (the session file records its cwd)
    r, _ = call("session/fork", {"sessionId": sid, "cwd": str(scratch/"workC"), "mcpServers": []})
    out["fork_other_cwd"] = (r.get("error") or {}).get("data", r.get("error") or "accepted")
    # fork in the same cwd, then /move the fork
    r, _ = call("session/fork", {"sessionId": sid, "cwd": str(scratch/"workA"), "mcpServers": []})
    out["fork_same_cwd_result_keys"] = list((r.get("result") or {}).keys()) or r.get("error")
    fsid = (r.get("result") or {}).get("sessionId")
    out["fork_sid_differs"] = fsid != sid
    if fsid:
        out["fork_dirs"] = prompt(fsid, "/dirs")[1]
        out["fork_move"] = prompt(fsid, f"/move {scratch/'workC'}")[1]
        out["fork_dirs_after_move"] = prompt(fsid, "/dirs")[1]
        res, txt = prompt(fsid, "read c.txt")
        out["fork_read_c"] = txt
        prompt(fsid, "say fork")
    r, _ = call("session/list", {})
    out["list"] = [(s.get("sessionId"), s.get("cwd"), s.get("title")) for s in (r.get("result") or {}).get("sessions", [])]
finally:
    p.terminate(); server.terminate()
(scratch/"wire.json").write_text(json.dumps(wire))
reqs = [json.loads(l) for l in log.read_text().splitlines()] if log.exists() else []
out["requests"] = [{"n": r["n_messages"], "last_user": r["last_user"], "sys_mentions_workB": str(scratch/"workB") in r["system"], "sys_mentions_workC": str(scratch/"workC") in r["system"]} for r in reqs]
# transcripts
tr = []
for f in sorted(glob.glob(str(scratch/"home/agent/sessions/**/*.jsonl"), recursive=True)):
    ls = [json.loads(x) for x in open(f) if x.strip().startswith("{")]
    hdr = next((x for x in ls if x.get("type") == "session"), {})
    tr.append({"file": os.path.basename(f), "cwd": hdr.get("cwd"), "parent": hdr.get("parentSession"), "messages": sum(1 for x in ls if x.get("type") == "message")})
out["transcripts"] = tr
print(json.dumps(out, indent=1))
