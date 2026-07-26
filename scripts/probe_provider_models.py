#!/usr/bin/env python3
"""Probe every model of every configured provider with a minimal chat request.

`available` in the jcode catalog only means "a key string exists", so this
sends a real (tiny) completion to find out which models actually answer.
"""
import json, os, sys, urllib.request, urllib.error, concurrent.futures, time

CFG = os.path.expanduser("~/Library/Application Support/jcode")

def envval(fname, key):
    p = os.path.join(CFG, fname)
    if not os.path.exists(p):
        return None
    for line in open(p):
        if line.startswith(key + "="):
            return line.split("=", 1)[1].strip()
    return None

PROVIDERS = {
    "openai":      ("https://api.openai.com/v1",                          envval("openai.env", "OPENAI_API_KEY")),
    "deepseek":    ("https://api.deepseek.com",                           envval("deepseek.env", "DEEPSEEK_API_KEY")),
    "moonshot":    ("https://api.moonshot.ai/v1",                         envval("moonshotai.env", "MOONSHOT_API_KEY")),
    "zai":         ("https://api.z.ai/api/paas/v4",                       envval("zai.env", "ZHIPU_API_KEY")),
    "minimax":     ("https://api.minimaxi.com/v1",                        envval("minimax.env", "MINIMAX_API_KEY")),
    "qwen":        ("https://dashscope.aliyuncs.com/compatible-mode/v1",  envval("openai-compatible.env", "OPENAI_COMPAT_API_KEY")),
    "siliconflow": ("https://api.siliconflow.cn/v1",                      envval("provider-siliconflow.env", "JCODE_PROVIDER_SILICONFLOW_API_KEY")),
    "modelscope":  ("https://api-inference.modelscope.cn/v1",             envval("provider-modelscope.env", "JCODE_PROVIDER_MODELSCOPE_API_KEY")),
}

SKIP = ("embedding", "whisper", "tts", "dall-e", "moderation", "audio", "image",
        "rerank", "babbage", "davinci", "sora", "realtime", "transcribe", "speech",
        "codex-mini", "video", "omni-moderation", "guard", "search-", "-vl-ocr")

def req(url, key, payload, timeout=45):
    body = json.dumps(payload).encode()
    r = urllib.request.Request(url, data=body, headers={
        "Authorization": "Bearer " + key, "Content-Type": "application/json"})
    return urllib.request.urlopen(r, timeout=timeout)

def list_models(name):
    base, key = PROVIDERS[name]
    if not key:
        return []
    try:
        r = urllib.request.Request(base + "/models", headers={"Authorization": "Bearer " + key})
        d = json.load(urllib.request.urlopen(r, timeout=30))
        ids = [m["id"] for m in d.get("data", [])]
        return [m for m in ids if not any(s in m.lower() for s in SKIP)]
    except Exception as e:
        print(f"  [{name}] catalog failed: {e}", file=sys.stderr)
        return []

def probe(name, model):
    base, key = PROVIDERS[name]
    payload = {"model": model, "messages": [{"role": "user", "content": "hi"}], "max_tokens": 4}
    try:
        resp = req(base + "/chat/completions", key, payload)
        json.load(resp)
        return (name, model, "OK", "")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()[:400]
        try:
            msg = json.loads(raw).get("error", {})
            msg = msg.get("message") if isinstance(msg, dict) else str(msg)
        except Exception:
            msg = raw
        msg = (msg or raw).replace("\n", " ")[:130]
        # Some models reject max_tokens but are otherwise alive; retry once.
        if "max_tokens" in msg or "max_completion_tokens" in msg:
            try:
                p2 = dict(payload); p2.pop("max_tokens")
                p2["max_completion_tokens"] = 16
                json.load(req(base + "/chat/completions", key, p2))
                return (name, model, "OK", "(needs max_completion_tokens)")
            except Exception as e2:
                return (name, model, f"FAIL {e.code}", str(e2)[:130])
        return (name, model, f"FAIL {e.code}", msg)
    except Exception as e:
        return (name, model, "FAIL", str(e)[:130])

def main():
    only = sys.argv[1:] or list(PROVIDERS)
    jobs = []
    for name in only:
        if not PROVIDERS.get(name) or not PROVIDERS[name][1]:
            print(f"  [{name}] no key configured", file=sys.stderr); continue
        models = list_models(name)
        print(f"  [{name}] {len(models)} chat models to probe", file=sys.stderr)
        jobs += [(name, m) for m in models]

    results = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=12) as ex:
        futs = {ex.submit(probe, n, m): (n, m) for n, m in jobs}
        for i, f in enumerate(concurrent.futures.as_completed(futs), 1):
            results.append(f.result())
            if i % 25 == 0:
                print(f"  ...{i}/{len(jobs)}", file=sys.stderr)
    # Merge into the durable report so probing one provider at a time
    # does not discard earlier providers' results.
    out = os.path.expanduser("~/.jcode/model-probe-report.json")
    os.makedirs(os.path.dirname(out), exist_ok=True)
    merged = {}
    if os.path.exists(out):
        for row in json.load(open(out)):
            merged[(row[0], row[1])] = row
    for row in results:
        merged[(row[0], row[1])] = list(row)
    json.dump(sorted(merged.values()), open(out, "w"), indent=1)
    print(f"\nreport: {out} ({len(merged)} models)", file=sys.stderr)

    ok = [r for r in results if r[2] == "OK"]
    bad = [r for r in results if r[2] != "OK"]
    print(f"\n=== WORKING: {len(ok)}/{len(results)} ===")
    for name in only:
        w = sorted(m for n, m, s, _ in ok if n == name)
        if w:
            print(f"\n{name} ({len(w)}):")
            for m in w:
                print("  ", m)
    print(f"\n=== FAILING: {len(bad)} ===")
    for n, m, s, msg in sorted(bad):
        print(f"  {n:12} {m:45} {s:9} {msg}")

main()
