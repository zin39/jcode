TOKEN=$(python3 -c "
import json, os
d=json.load(open(os.path.expanduser('~/Library/Application Support/jcode/auth.json')))
print(next(a['access'] for a in d['anthropic_accounts'] if a['label']=='claude-14'))")

echo "token prefix: ${TOKEN:0:20}"
echo "=== FULL RESPONSE ==="
curl -s -D - https://api.anthropic.com/v1/messages \
  -H "Authorization: Bearer $TOKEN" \
  -H "anthropic-beta: oauth-2025-04-20" \
  -H "anthropic-version: 2023-06-01" \
  -H "content-type: application/json" \
  -d '{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}'
