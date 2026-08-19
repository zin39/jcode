check_acct() {
  local LABEL=$1
  local TOKEN=$(python3 -c "
import json
d=json.load(open('$HOME/Library/Application Support/jcode/auth.json'))
print(next((a['access'] for a in d['anthropic_accounts'] if a['label']=='$LABEL'),''))")
  curl -s -D - -o /dev/null https://api.anthropic.com/v1/messages \
    -H "Authorization: Bearer $TOKEN" \
    -H "anthropic-beta: oauth-2025-04-20" \
    -H "anthropic-version: 2023-06-01" \
    -H "content-type: application/json" \
    -d '{"model":"claude-haiku-4-5-20251001","max_tokens":1,"messages":[{"role":"user","content":"hi"}]}' \
  | grep -iE "unified-(5h|7d|status).*(reset|status|util)" \
  | while read k v; do
      case "$k" in *reset:) printf "%-40s %s\n" "$k" "$(date -r $v -u '+%Y-%m-%d %H:%M UTC')";; *) echo "$k $v";; esac
    done
}
check_acct claude-8
