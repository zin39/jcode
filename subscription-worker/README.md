# jcode Subscription Worker

Cloudflare Worker (`subscription-api`) backing the jcode token subscription
plan. One worker handles accounts + magic-link CLI auth, API keys, the Stripe
webhook, and an OpenAI-compatible metered model router.

- **Code**: `src/worker.js` (routing, D1, proxying), `src/lib.js` (pure logic:
  hashing, Stripe signatures, pricing, budget math, OpenAI↔Anthropic
  translation - fully unit-tested).
- **Storage**: D1 database `jcode-subscriptions` (`schema.sql`).
- **Domain**: intended for `api.solosystems.dev` (route commented out in
  `wrangler.toml` until DNS is ready).

## Endpoints

### Auth (device-code + magic link)

| Endpoint | Description |
|---|---|
| `POST /v1/auth/device` | Body `{email}`. Creates a pending auth, emails a magic link via Resend (from `login@solosystems.dev`). Returns `{device_code, verify_url, expires_in, interval}`. Device codes expire after 15 minutes. |
| `GET /v1/auth/verify?token=...` | Magic-link target. Marks the device code approved and shows a minimal HTML success page. |
| `POST /v1/auth/token` | Body `{device_code}`. Poll at `interval` seconds. `428 authorization_pending` until the link is clicked, then returns `{api_key, account_id, email, tier}` once (the code is consumed). Creates the account row if new (tier `none`). |

### Account and keys (Bearer `jck_live_...`)

| Endpoint | Description |
|---|---|
| `GET /v1/me` | `{account_id, email, tier, status, usage: {used_usd, budget_usd, resets_at}}`. `used_usd` is a `SUM(cost_usd)` over the current UTC calendar month. |
| `POST /v1/keys/rotate` | Revokes the calling key, mints and returns a fresh one: `{api_key, account_id}`. |

### Billing

| Endpoint | Description |
|---|---|
| `POST /v1/stripe/webhook` | Stripe webhook. Signature verified manually (HMAC-SHA256 of `t.body` against `Stripe-Signature` `v1` entries, 5-minute timestamp tolerance, constant-time compare). Handles `checkout.session.completed` (link customer / create account), `customer.subscription.updated` (set tier from `PRICE_ID_PLUS`/`PRICE_ID_FLAGSHIP` and status), `customer.subscription.deleted` (tier `none`, status `canceled`). |

### Waitlist

| Endpoint | Description |
|---|---|
| `POST /v1/waitlist` | Body `{email, tier: 'plus'\|'flagship', note?}` (note max 500 chars). Upserts into the `waitlist` table (repeat signups update tier/note, keep original status and `created_at`) and fires a best-effort notification email to `jeremy@solosystems.dev` via `ctx.waitUntil` (subject `jcode waitlist: <tier> signup`; a failed email never fails the signup). Returns `{ok: true}`. CORS: `POST`/`OPTIONS` allowed only from `https://solosystems.dev` and `https://solosystems.pages.dev` (the only browser-facing endpoint; everything else is CLI/webhook traffic). |

### Router (OpenAI-compatible)

| Endpoint | Description |
|---|---|
| `GET /v1/models` | Curated catalog with tier annotations. |
| `POST /v1/chat/completions` | Authenticated proxy. Checks tier access, budget, and rate limit, then streams. Every response carries an `X-Request-Id` header (uuid, also logged). |
| `GET /v1/health` | `{ok, db_size_bytes}`. |

Model catalog and routing:

| Model | Upstream | Tiers |
|---|---|---|
| `claude-opus-4-8` | Anthropic `/v1/messages` (translated) | plus, flagship |
| `gpt-5.5` | OpenAI `/v1/chat/completions` (passthrough) | plus, flagship |
| `claude-fable-5` | Anthropic (translated) | flagship |
| `gpt-5.6-sol` | OpenAI (passthrough) | flagship |

Streaming is a true SSE passthrough via `TransformStream` (never buffered).
For `claude-*` models the OpenAI-format request is translated to Anthropic
Messages (system prompts, tools, tool results, sampling params) and the
Anthropic SSE stream is translated back into OpenAI
`chat.completion.chunk` events, ending with a usage-bearing final chunk and
`data: [DONE]`. For `gpt-*` streams, `stream_options.include_usage` is
injected so the tail chunk carries usage.

## Metering and budgets

- Usage is parsed from the upstream stream tail (`message_start` +
  `message_delta` for Anthropic, the final usage chunk for OpenAI) and
  recorded into `usage_events` via `ctx.waitUntil`, so metering never blocks
  the stream.
- Cost comes from the single hardcoded `PRICES_PER_MTOK` table in
  `src/lib.js` (USD per million tokens: input / output / cache_read /
  cache_write).
- Monthly budgets: **plus = $18.00**, **flagship = $3000.00**. The window is
  the current UTC calendar month; it resets at the next month boundary
  (`resets_at` in `/v1/me`).
- Hard cutoff: when `SUM(cost_usd)` for the window reaches the budget,
  `POST /v1/chat/completions` returns
  `402 {"error":{"code":"budget_exhausted","used_usd":...,"budget_usd":...,"resets_at":...}}`.
  Note the check happens before the request, so one in-flight request can
  slightly overshoot the budget. That overshoot is bounded by one request's
  cost and is accepted for v1.

## Rate limits

Sliding 60-second window per API key, backed by a D1 `rate_events` table:
**60 req/min (plus)**, **300 req/min (flagship)**. Exceeding it returns `429
rate_limited`.

v1 limitation: the counter read and the event write are not atomic, so a
burst of parallel requests can briefly exceed the limit, and each request
costs one extra D1 read + write. Good enough for launch; move to Durable
Objects (one object per key) if precision or latency matters.

## API key format

`jck_live_<40 lowercase hex>` (20 random bytes). Only the SHA-256 hex digest
of the full key string is stored (`keys.key_hash`). Keys are shown once at
issuance (`/v1/auth/token`, `/v1/keys/rotate`) and cannot be recovered, only
rotated.

## Setup

```bash
npm install

# 1. Create the database and put its id in wrangler.toml
npx wrangler d1 create jcode-subscriptions

# 2. Initialize schema
npm run migrate            # remote
npm run migrate:local      # local dev

# 3. Secrets
npx wrangler secret put RESEND_API_KEY        # Resend API key (magic-link email)
npx wrangler secret put STRIPE_WEBHOOK_SECRET # whsec_... from the Stripe webhook endpoint
npx wrangler secret put ANTHROPIC_API_KEY     # upstream key for claude-* models
npx wrangler secret put OPENAI_API_KEY        # upstream key for gpt-* models

# 4. Vars (wrangler.toml): PRICE_ID_PLUS, PRICE_ID_FLAGSHIP, PUBLIC_BASE_URL

# 5. Deploy
npm run deploy
```

### Stripe setup

1. Create two recurring prices (Plus, Flagship) in the Stripe dashboard and
   put their `price_...` ids into `PRICE_ID_PLUS` / `PRICE_ID_FLAGSHIP` in
   `wrangler.toml`. The current test-mode IDs (sandbox
   `acct_1TpkGjDDRk0ghBLm`) are:

   ```toml
   PRICE_ID_PLUS = "price_1TpkQYDdPoMy1kBxXlfk6HfN"
   PRICE_ID_FLAGSHIP = "price_1TpkQYDdPoMy1kBxjyBQPOZt"
   ```

2. Add a webhook endpoint pointing at
   `https://<worker-host>/v1/stripe/webhook` subscribed to:
   `checkout.session.completed`, `customer.subscription.updated`,
   `customer.subscription.deleted`.
3. Copy the endpoint's signing secret into the `STRIPE_WEBHOOK_SECRET`
   secret.
4. Sell via Checkout / Payment Links using the customer's email; the
   `checkout.session.completed` event links (or creates) the account by
   email, and the subscription events set tier/status.

#### Stripe API best practices

- **Production keys**: use a Stripe restricted key (`rk_live_...`) scoped to
  only Checkout Sessions, Subscriptions, and Customers rather than a full
  secret key (`sk_live_...`). Never commit any Stripe key.
- **API version pinning**: pin `Stripe-Version: 2026-06-24.dahlia` on all
  direct Stripe API calls so dashboard version upgrades cannot silently
  change response shapes.
- **Checkout Sessions**: any Checkout Session creation must use
  `mode='subscription'` and must NOT pass `payment_method_types` (let Stripe
  Dashboard payment-method settings and dynamic payment methods decide).
  Example:

  ```bash
  curl https://api.stripe.com/v1/checkout/sessions \
    -u "$STRIPE_RESTRICTED_KEY:" \
    -H "Stripe-Version: 2026-06-24.dahlia" \
    -d mode=subscription \
    -d "line_items[0][price]"=price_1TpkQYDdPoMy1kBxXlfk6HfN \
    -d "line_items[0][quantity]"=1 \
    -d customer_email="user@example.com" \
    -d success_url="https://jcode.dev/thanks" \
    -d cancel_url="https://jcode.dev/pricing"
  ```

- **Local webhook testing**: forward events to the local dev worker with

  ```bash
  stripe listen --forward-to localhost:8787/v1/stripe/webhook
  ```

  and use the `whsec_...` it prints as `STRIPE_WEBHOOK_SECRET` for the local
  run.

### Resend setup

Verify the `solosystems.dev` domain in Resend so mail can be sent from
`login@solosystems.dev`, then set `RESEND_API_KEY`.

## Testing

```bash
npm test
```

Plain `node --test`, no framework. Covers key generation/hashing, Stripe
signature verification (valid, tampered, stale, multi-`v1`), cost
computation against the price table, budget window math (month rollover),
SSE parsing across chunk boundaries, OpenAI→Anthropic request translation
(system/tools/tool results), Anthropic→OpenAI stream chunk translation
(text and tool_use), and fetch-mocked end-to-end proxy tests asserting
translation, passthrough, metering writes, 402/403/429 enforcement.

## Ops queries

Run with `npx wrangler d1 execute jcode-subscriptions --remote --command "..."`.

Revenue vs cost per account, current month (revenue assumes plus=$20,
flagship=$200 list prices; adjust to actual):

```sql
SELECT a.email, a.tier,
       CASE a.tier WHEN 'plus' THEN 20.0 WHEN 'flagship' THEN 200.0 ELSE 0 END AS revenue_usd,
       ROUND(COALESCE(SUM(u.cost_usd), 0), 4) AS cost_usd,
       ROUND(CASE a.tier WHEN 'plus' THEN 20.0 WHEN 'flagship' THEN 200.0 ELSE 0 END
             - COALESCE(SUM(u.cost_usd), 0), 4) AS margin_usd
FROM accounts a
LEFT JOIN usage_events u
  ON u.account_id = a.account_id
 AND u.created_at >= strftime('%Y-%m-01 00:00:00', 'now')
GROUP BY a.account_id
ORDER BY cost_usd DESC;
```

Top spenders this month:

```sql
SELECT a.email, a.tier, COUNT(*) AS requests,
       SUM(u.input_tokens) AS in_tok, SUM(u.output_tokens) AS out_tok,
       ROUND(SUM(u.cost_usd), 4) AS cost_usd
FROM usage_events u JOIN accounts a ON a.account_id = u.account_id
WHERE u.created_at >= strftime('%Y-%m-01 00:00:00', 'now')
GROUP BY u.account_id
ORDER BY cost_usd DESC
LIMIT 20;
```

Accounts near/over budget exhaustion (this month):

```sql
SELECT a.email, a.tier,
       ROUND(SUM(u.cost_usd), 4) AS used_usd,
       CASE a.tier WHEN 'plus' THEN 18.0 WHEN 'flagship' THEN 3000.0 ELSE 0 END AS budget_usd,
       ROUND(100.0 * SUM(u.cost_usd) /
             CASE a.tier WHEN 'plus' THEN 18.0 WHEN 'flagship' THEN 3000.0 ELSE 1 END, 1) AS pct_used
FROM usage_events u JOIN accounts a ON a.account_id = u.account_id
WHERE u.created_at >= strftime('%Y-%m-01 00:00:00', 'now')
GROUP BY u.account_id
HAVING pct_used >= 80
ORDER BY pct_used DESC;
```

Cost by model this month:

```sql
SELECT model, COUNT(*) AS requests,
       SUM(input_tokens) AS in_tok, SUM(output_tokens) AS out_tok,
       SUM(cache_read_tokens) AS cache_read, SUM(cache_write_tokens) AS cache_write,
       ROUND(SUM(cost_usd), 4) AS cost_usd
FROM usage_events
WHERE created_at >= strftime('%Y-%m-01 00:00:00', 'now')
GROUP BY model ORDER BY cost_usd DESC;
```

Stale pending device auths (should stay small; rows are only consumed, not
pruned, in v1):

```sql
SELECT status, COUNT(*) FROM device_auth GROUP BY status;
DELETE FROM device_auth WHERE expires_at < datetime('now', '-1 day');
```

Rate-limit table hygiene (pruned lazily in the worker; manual prune):

```sql
DELETE FROM rate_events WHERE at_ms < (strftime('%s','now') - 300) * 1000;
```

Waitlist signups by tier:

```sql
SELECT tier, status, COUNT(*) AS signups
FROM waitlist GROUP BY tier, status ORDER BY tier, status;
```

Daily waitlist signups (last 30 days):

```sql
SELECT date(created_at) AS day, tier, COUNT(*) AS signups
FROM waitlist
WHERE created_at >= datetime('now', '-30 days')
GROUP BY day, tier ORDER BY day DESC;
```

Pending waitlist count (people to invite):

```sql
SELECT COUNT(*) AS pending FROM waitlist WHERE status = 'pending';
```
