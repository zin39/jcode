import json, sys, subprocess, datetime, os

AUTH = os.path.expanduser("~/Library/Application Support/jcode/auth.json")

def check(label):
    accts = json.load(open(AUTH)).get("anthropic_accounts", [])
    tok = next((a["access"] for a in accts if a["label"] == label), None)
    if not tok:
        print(f"{label}: not found"); return
    out = subprocess.run([
        "curl", "-s", "-D", "-", "-o", "/dev/null",
        "https://api.anthropic.com/v1/messages",
        "-H", f"Authorization: Bearer {tok}",
        "-H", "anthropic-beta: oauth-2025-04-20",
        "-H", "anthropic-version: 2023-06-01",
        "-H", "content-type: application/json",
        "-d", '{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}',
    ], capture_output=True, text=True).stdout

    hdr = {}
    for line in out.splitlines():
        if ":" in line:
            k, _, v = line.partition(":")
            hdr[k.strip().lower()] = v.strip()

    status = hdr.get("anthropic-ratelimit-unified-status", "?")
    now = datetime.datetime.now(datetime.timezone.utc)
    print(f"\n{label}: {status}")
    for win in ("5h", "7d"):
        st = hdr.get(f"anthropic-ratelimit-unified-{win}-status", "?")
        rs = hdr.get(f"anthropic-ratelimit-unified-{win}-reset")
        if rs and rs.isdigit():
            dt = datetime.datetime.fromtimestamp(int(rs), datetime.timezone.utc)
            h = (dt - now).total_seconds() / 3600
            print(f"  {win}: {st:9} resets {dt:%Y-%m-%d %H:%M UTC} (in {h:.1f} h)")
        else:
            print(f"  {win}: {st}")

if __name__ == "__main__":
    labels = sys.argv[1:] or [f"claude-{i}" for i in range(1, 13)]
    for l in labels:
        check(l)
