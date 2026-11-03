#!/usr/bin/env python3
"""summarize.py RECDIR [--full] : compact view of recorded OTLP requests."""
import json, os, sys, glob

def val(v):
    if not isinstance(v, dict): return v
    for k in ("stringValue", "intValue", "doubleValue", "boolValue"):
        if k in v: return v[k]
    if "arrayValue" in v: return [val(x) for x in v["arrayValue"].get("values", [])]
    if "kvlistValue" in v: return {a["key"]: val(a["value"]) for a in v["kvlistValue"].get("values", [])}
    return v

def attrs(lst): return {a["key"]: val(a["value"]) for a in (lst or [])}

def short(d, maxlen=90):
    return {k: (v if not isinstance(v, str) or len(v) <= maxlen else v[:maxlen] + "…") for k, v in d.items()}

full = "--full" in sys.argv
for f in sorted(glob.glob(os.path.join(sys.argv[1], "*-*.json"))):
    r = json.load(open(f))
    b = r.get("body")
    print(f"=== {os.path.basename(f)}  {r['path']}  {r['content_type']}  enc={r['content_encoding']} {r['wire_bytes']}B->{r['decoded_bytes']}B  UA={r['headers'].get('User-Agent') or r['headers'].get('user-agent')}")
    if not b: print("   ", r.get("decode_error") or r.get("note")); continue
    for key in ("resourceSpans", "resourceLogs", "resourceMetrics"):
        for rs in b.get(key, []):
            print("  resource:", short(attrs(rs.get("resource", {}).get("attributes"))))
            for ss in rs.get("scopeSpans", []) + rs.get("scopeLogs", []) + rs.get("scopeMetrics", []):
                print("  scope:", ss.get("scope", {}).get("name"), ss.get("scope", {}).get("version"))
                for s in ss.get("spans", []):
                    a = attrs(s.get("attributes"))
                    print(f"   span {s['name']} id={s['spanId'][:6]} parent={(s.get('parentSpanId') or '-')[:6]} kind={s.get('kind')}", json.dumps(a if full else short(a), default=str)[:1500])
                    for e in s.get("events", []): print("      event", e["name"], json.dumps(short(attrs(e.get("attributes"))), default=str)[:300])
                for lr in ss.get("logRecords", []):
                    a = attrs(lr.get("attributes"))
                    print("   log", val(lr.get("body")), json.dumps(a if full else short(a), default=str)[:1500])
                for m in ss.get("metrics", []):
                    for kind in ("sum", "gauge", "histogram", "exponentialHistogram"):
                        if kind in m:
                            for dp in m[kind].get("dataPoints", []):
                                v = dp.get("asInt", dp.get("asDouble", {k: dp.get(k) for k in ("count", "sum") if k in dp}))
                                print(f"   metric {m['name']} [{m.get('unit','')}] {kind} = {v}", json.dumps(short(attrs(dp.get("attributes"))), default=str)[:500])
