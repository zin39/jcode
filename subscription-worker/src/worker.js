// subscription-api: accounts, magic-link device auth, API keys, Stripe
// webhook, and an OpenAI-compatible metered router for the jcode token
// subscription plan.
//
// Secrets: RESEND_API_KEY, STRIPE_WEBHOOK_SECRET, ANTHROPIC_API_KEY,
// OPENAI_API_KEY. Vars: PRICE_ID_PLUS, PRICE_ID_FLAGSHIP, PUBLIC_BASE_URL.

import {
  API_KEY_RE,
  AnthropicToOpenAIStreamTranslator,
  MODELS,
  SseParser,
  TIER_BUDGET_USD,
  TIER_RATE_LIMIT_PER_MIN,
  budgetWindow,
  computeCostUsd,
  generateApiKey,
  hashApiKey,
  isValidEmail,
  openaiToAnthropicRequest,
  sqliteUtc,
  usageFromOpenAIChunk,
  validateWaitlistSignup,
  verifyStripeSignature,
} from "./lib.js";

const DEVICE_CODE_TTL_SECS = 15 * 60;
const DEVICE_POLL_INTERVAL_SECS = 5;
const MAGIC_LINK_FROM = "jcode <login@solosystems.dev>";
const WAITLIST_NOTIFY_TO = "jeremy@solosystems.dev";
const WAITLIST_NOTIFY_FROM = "jcode <login@solosystems.dev>";

// Browser origins allowed to POST /v1/waitlist. This is the only
// cross-origin browser surface; everything else is CLI/webhook traffic
// with no CORS needs.
const WAITLIST_ALLOWED_ORIGINS = [
  "https://solosystems.dev",
  "https://solosystems.pages.dev",
];

const ANTHROPIC_URL = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION = "2023-06-01";
const OPENAI_URL = "https://api.openai.com/v1/chat/completions";

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const route = `${request.method} ${url.pathname}`;
    try {
      switch (route) {
        case "POST /v1/auth/device":
          return await handleAuthDevice(request, env);
        case "GET /v1/auth/verify":
          return await handleAuthVerify(url, env);
        case "POST /v1/auth/token":
          return await handleAuthToken(request, env);
        case "GET /v1/me":
          return await handleMe(request, env);
        case "POST /v1/keys/rotate":
          return await handleRotateKey(request, env);
        case "POST /v1/stripe/webhook":
          return await handleStripeWebhook(request, env);
        case "POST /v1/waitlist":
          return await handleWaitlist(request, env, ctx);
        case "OPTIONS /v1/waitlist":
          return waitlistPreflight(request);
        case "GET /v1/models":
          return await handleModels(request, env);
        case "POST /v1/chat/completions":
          return await handleChatCompletions(request, env, ctx);
        case "GET /v1/health":
          return await handleHealth(env);
        default:
          return errorResponse(404, "not_found", `no route for ${route}`);
      }
    } catch (err) {
      console.error(`unhandled error on ${route}:`, err);
      return errorResponse(500, "internal_error", "internal error");
    }
  },
};

// ---------------------------------------------------------------------------
// Auth: device-code + magic-link email
// ---------------------------------------------------------------------------

async function handleAuthDevice(request, env) {
  const body = await readJson(request);
  const email = String(body?.email || "").trim().toLowerCase();
  if (!isValidEmail(email)) {
    return errorResponse(400, "invalid_email", "a valid email is required");
  }

  const deviceCode = crypto.randomUUID();
  const verifyToken = crypto.randomUUID();
  const expiresAt = sqliteUtc(new Date(Date.now() + DEVICE_CODE_TTL_SECS * 1000));

  await env.DB.prepare(
    `INSERT INTO device_auth (device_code, verify_token, email, status, expires_at)
     VALUES (?, ?, ?, 'pending', ?)`,
  )
    .bind(deviceCode, verifyToken, email, expiresAt)
    .run();

  const baseUrl = (env.PUBLIC_BASE_URL || "").replace(/\/$/, "");
  const verifyUrl = `${baseUrl}/v1/auth/verify?token=${verifyToken}`;
  await sendMagicLinkEmail(env, email, verifyUrl);

  return jsonResponse({
    device_code: deviceCode,
    verify_url: verifyUrl,
    expires_in: DEVICE_CODE_TTL_SECS,
    interval: DEVICE_POLL_INTERVAL_SECS,
  });
}

async function sendMagicLinkEmail(env, email, verifyUrl) {
  const res = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.RESEND_API_KEY}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      from: MAGIC_LINK_FROM,
      to: [email],
      subject: "Sign in to jcode",
      html: `<p>Click the link below to sign in to jcode:</p>
<p><a href="${verifyUrl}">Sign in to jcode</a></p>
<p>This link expires in 15 minutes. If you didn't request it, ignore this email.</p>`,
    }),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    console.error(`resend send failed: ${res.status} ${text}`);
    throw new Error("failed to send magic-link email");
  }
}

async function handleAuthVerify(url, env) {
  const token = url.searchParams.get("token") || "";
  const now = sqliteUtc(new Date());
  const row = await env.DB.prepare(
    `SELECT device_code, status, expires_at FROM device_auth WHERE verify_token = ?`,
  )
    .bind(token)
    .first();

  if (!row || row.expires_at < now) {
    return htmlResponse(
      "Link expired",
      "This sign-in link is invalid or has expired. Please request a new one from the jcode CLI.",
      400,
    );
  }
  if (row.status === "pending") {
    await env.DB.prepare(
      `UPDATE device_auth SET status = 'approved', approved_at = datetime('now')
       WHERE verify_token = ? AND status = 'pending'`,
    )
      .bind(token)
      .run();
  }
  return htmlResponse(
    "You're signed in",
    "Sign-in approved. You can close this tab and return to your terminal.",
  );
}

async function handleAuthToken(request, env) {
  const body = await readJson(request);
  const deviceCode = String(body?.device_code || "");
  if (!deviceCode) {
    return errorResponse(400, "invalid_request", "device_code is required");
  }

  const now = sqliteUtc(new Date());
  const row = await env.DB.prepare(
    `SELECT device_code, email, status, expires_at FROM device_auth WHERE device_code = ?`,
  )
    .bind(deviceCode)
    .first();

  if (!row || row.expires_at < now) {
    return errorResponse(400, "expired_token", "device code is invalid or expired");
  }
  if (row.status === "pending") {
    return errorResponse(428, "authorization_pending", "user has not approved yet");
  }
  if (row.status === "consumed") {
    return errorResponse(400, "expired_token", "device code already used");
  }

  // Approved: consume it, upsert the account, mint a key.
  await env.DB.prepare(
    `UPDATE device_auth SET status = 'consumed' WHERE device_code = ? AND status = 'approved'`,
  )
    .bind(deviceCode)
    .run();

  let account = await env.DB.prepare(`SELECT * FROM accounts WHERE email = ?`)
    .bind(row.email)
    .first();
  if (!account) {
    const accountId = crypto.randomUUID();
    await env.DB.prepare(
      `INSERT INTO accounts (account_id, email, tier, status) VALUES (?, ?, 'none', 'active')`,
    )
      .bind(accountId, row.email)
      .run();
    account = { account_id: accountId, email: row.email, tier: "none", status: "active" };
  }

  const apiKey = await mintKey(env, account.account_id);
  return jsonResponse({
    api_key: apiKey,
    account_id: account.account_id,
    email: account.email,
    tier: account.tier,
  });
}

async function mintKey(env, accountId) {
  const apiKey = generateApiKey(crypto.getRandomValues(new Uint8Array(20)));
  const keyHash = await hashApiKey(apiKey);
  await env.DB.prepare(
    `INSERT INTO keys (key_id, account_id, key_hash) VALUES (?, ?, ?)`,
  )
    .bind(crypto.randomUUID(), accountId, keyHash)
    .run();
  return apiKey;
}

// ---------------------------------------------------------------------------
// Bearer auth helper
// ---------------------------------------------------------------------------

async function authenticate(request, env) {
  const header = request.headers.get("Authorization") || "";
  const match = header.match(/^Bearer\s+(.+)$/i);
  if (!match) return { error: errorResponse(401, "unauthorized", "missing bearer token") };
  const key = match[1].trim();
  if (!API_KEY_RE.test(key)) {
    return { error: errorResponse(401, "unauthorized", "malformed API key") };
  }
  const keyHash = await hashApiKey(key);
  const row = await env.DB.prepare(
    `SELECT k.key_id, a.account_id, a.email, a.tier, a.status
     FROM keys k JOIN accounts a ON a.account_id = k.account_id
     WHERE k.key_hash = ? AND k.revoked_at IS NULL`,
  )
    .bind(keyHash)
    .first();
  if (!row) return { error: errorResponse(401, "unauthorized", "unknown or revoked API key") };
  return { auth: row };
}

// ---------------------------------------------------------------------------
// /v1/me and key rotation
// ---------------------------------------------------------------------------

async function handleMe(request, env) {
  const { auth, error } = await authenticate(request, env);
  if (error) return error;

  const { start, resetsAt } = budgetWindow();
  const usedRow = await env.DB.prepare(
    `SELECT COALESCE(SUM(cost_usd), 0) AS used FROM usage_events
     WHERE account_id = ? AND created_at >= ?`,
  )
    .bind(auth.account_id, sqliteUtc(start))
    .first();

  return jsonResponse({
    account_id: auth.account_id,
    email: auth.email,
    tier: auth.tier,
    status: auth.status,
    usage: {
      used_usd: round6(usedRow?.used || 0),
      budget_usd: TIER_BUDGET_USD[auth.tier] ?? 0,
      resets_at: resetsAt.toISOString(),
    },
  });
}

async function handleRotateKey(request, env) {
  const { auth, error } = await authenticate(request, env);
  if (error) return error;

  await env.DB.prepare(
    `UPDATE keys SET revoked_at = datetime('now') WHERE key_id = ?`,
  )
    .bind(auth.key_id)
    .run();
  const apiKey = await mintKey(env, auth.account_id);
  return jsonResponse({ api_key: apiKey, account_id: auth.account_id });
}

// ---------------------------------------------------------------------------
// Stripe webhook
// ---------------------------------------------------------------------------

async function handleStripeWebhook(request, env) {
  const rawBody = await request.text();
  const ok = await verifyStripeSignature(
    rawBody,
    request.headers.get("Stripe-Signature"),
    env.STRIPE_WEBHOOK_SECRET,
  );
  if (!ok) return errorResponse(400, "invalid_signature", "webhook signature verification failed");

  const event = JSON.parse(rawBody);
  const obj = event.data?.object || {};

  switch (event.type) {
    case "checkout.session.completed": {
      const email = (obj.customer_details?.email || obj.customer_email || "").toLowerCase();
      const customerId = obj.customer;
      if (!email) break;
      // Upsert account, attach stripe customer. Tier is set by the
      // subsequent customer.subscription.updated event; but if line items
      // are absent we still mark the customer linkage here.
      let account = await env.DB.prepare(`SELECT account_id FROM accounts WHERE email = ?`)
        .bind(email)
        .first();
      if (!account) {
        const accountId = crypto.randomUUID();
        await env.DB.prepare(
          `INSERT INTO accounts (account_id, email, tier, status, stripe_customer_id)
           VALUES (?, ?, 'none', 'active', ?)`,
        )
          .bind(accountId, email, customerId)
          .run();
      } else {
        await env.DB.prepare(
          `UPDATE accounts SET stripe_customer_id = ? WHERE account_id = ?`,
        )
          .bind(customerId, account.account_id)
          .run();
      }
      break;
    }
    case "customer.subscription.updated": {
      const customerId = obj.customer;
      const tier = tierFromSubscription(obj, env);
      const status = accountStatusFromStripe(obj.status);
      await env.DB.prepare(
        `UPDATE accounts SET tier = ?, status = ? WHERE stripe_customer_id = ?`,
      )
        .bind(tier, status, customerId)
        .run();
      break;
    }
    case "customer.subscription.deleted": {
      await env.DB.prepare(
        `UPDATE accounts SET tier = 'none', status = 'canceled' WHERE stripe_customer_id = ?`,
      )
        .bind(obj.customer)
        .run();
      break;
    }
    default:
      // Acknowledge unhandled event types so Stripe stops retrying.
      break;
  }
  return jsonResponse({ received: true });
}

function tierFromSubscription(subscription, env) {
  const priceIds = (subscription.items?.data || []).map((item) => item.price?.id);
  if (priceIds.includes(env.PRICE_ID_FLAGSHIP)) return "flagship";
  if (priceIds.includes(env.PRICE_ID_PLUS)) return "plus";
  return "none";
}

function accountStatusFromStripe(stripeStatus) {
  switch (stripeStatus) {
    case "active":
    case "trialing":
      return "active";
    case "past_due":
    case "unpaid":
      return "past_due";
    default:
      return "canceled";
  }
}

// ---------------------------------------------------------------------------
// Waitlist: public signup form on solosystems.dev (pricing page).
// ---------------------------------------------------------------------------

function waitlistCorsHeaders(request) {
  const origin = request.headers.get("Origin");
  if (!origin || !WAITLIST_ALLOWED_ORIGINS.includes(origin)) return {};
  return {
    "Access-Control-Allow-Origin": origin,
    "Access-Control-Allow-Methods": "POST, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type",
    "Access-Control-Max-Age": "86400",
    Vary: "Origin",
  };
}

function waitlistPreflight(request) {
  return new Response(null, { status: 204, headers: waitlistCorsHeaders(request) });
}

async function handleWaitlist(request, env, ctx) {
  const cors = waitlistCorsHeaders(request);
  const body = await readJson(request);
  const parsed = validateWaitlistSignup(body);
  if (parsed.error) {
    return errorResponse(400, parsed.error.code, parsed.error.message, {}, cors);
  }
  const { email, tier, note } = parsed;
  const referrer = request.headers.get("Referer") || null;

  // Upsert: repeat signups update tier/note but keep the original status
  // and created_at (someone already invited stays invited).
  await env.DB.prepare(
    `INSERT INTO waitlist (email, tier, note, referrer)
     VALUES (?, ?, ?, ?)
     ON CONFLICT(email) DO UPDATE SET
       tier = excluded.tier,
       note = COALESCE(excluded.note, waitlist.note)`,
  )
    .bind(email, tier, note, referrer)
    .run();

  // Notify, best-effort: a failed email must never fail the signup.
  ctx.waitUntil(
    sendWaitlistNotification(env, { email, tier, note }).catch((err) =>
      console.error("waitlist notification failed:", err),
    ),
  );

  return jsonResponse({ ok: true }, 200, cors);
}

async function sendWaitlistNotification(env, { email, tier, note }) {
  const res = await fetch("https://api.resend.com/emails", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.RESEND_API_KEY}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      from: WAITLIST_NOTIFY_FROM,
      to: [WAITLIST_NOTIFY_TO],
      subject: `jcode waitlist: ${tier} signup`,
      html: `<p><strong>${escapeHtml(email)}</strong> joined the <strong>${escapeHtml(tier)}</strong> waitlist.</p>${
        note ? `<p>Note: ${escapeHtml(note)}</p>` : ""
      }`,
    }),
  });
  if (!res.ok) {
    const text = await res.text().catch(() => "");
    throw new Error(`resend send failed: ${res.status} ${text.slice(0, 200)}`);
  }
}

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

// ---------------------------------------------------------------------------
// Models list
// ---------------------------------------------------------------------------

async function handleModels(request, env) {
  // Public catalog; if authed, could filter by tier, but the router enforces
  // tier at request time, so we return the full curated list with tier info.
  const data = Object.entries(MODELS).map(([id, m]) => ({
    id,
    object: "model",
    owned_by: m.provider,
    tiers: m.tiers,
  }));
  return jsonResponse({ object: "list", data });
}

// ---------------------------------------------------------------------------
// Chat completions router
// ---------------------------------------------------------------------------

async function handleChatCompletions(request, env, ctx) {
  const { auth, error } = await authenticate(request, env);
  if (error) return error;

  if (auth.status !== "active") {
    return errorResponse(403, "account_inactive", `account status is ${auth.status}`);
  }
  const body = await readJson(request);
  if (!body || typeof body !== "object") {
    return errorResponse(400, "invalid_request", "JSON body required");
  }
  const modelId = String(body.model || "");
  const modelInfo = MODELS[modelId];
  if (!modelInfo) {
    return errorResponse(404, "model_not_found", `unknown model: ${modelId}`);
  }
  if (!modelInfo.tiers.includes(auth.tier)) {
    return errorResponse(
      403,
      "model_not_allowed",
      `model ${modelId} requires tier ${modelInfo.tiers.join(" or ")}, account tier is ${auth.tier}`,
    );
  }

  // Rate limit: sliding 60s window per key, backed by D1. See README for the
  // v1 limitation discussion.
  const limited = await checkRateLimit(env, ctx, auth.key_id, auth.tier);
  if (limited) return limited;

  // Budget check (hard cutoff).
  const { start, resetsAt } = budgetWindow();
  const budget = TIER_BUDGET_USD[auth.tier] ?? 0;
  const usedRow = await env.DB.prepare(
    `SELECT COALESCE(SUM(cost_usd), 0) AS used FROM usage_events
     WHERE account_id = ? AND created_at >= ?`,
  )
    .bind(auth.account_id, sqliteUtc(start))
    .first();
  const usedUsd = usedRow?.used || 0;
  if (usedUsd >= budget) {
    return errorResponse(402, "budget_exhausted", "monthly token budget exhausted", {
      used_usd: round6(usedUsd),
      budget_usd: budget,
      resets_at: resetsAt.toISOString(),
    });
  }

  const requestId = crypto.randomUUID();
  console.log(
    `request ${requestId}: account=${auth.account_id} model=${modelId} tier=${auth.tier} stream=${Boolean(body.stream)}`,
  );

  const meter = (usage) =>
    recordUsage(env, {
      accountId: auth.account_id,
      requestId,
      model: modelId,
      usage,
    });

  if (modelInfo.provider === "anthropic") {
    return proxyAnthropic(env, ctx, body, modelId, requestId, meter);
  }
  return proxyOpenAI(env, ctx, body, requestId, meter);
}

async function recordUsage(env, { accountId, requestId, model, usage }) {
  try {
    const costUsd = computeCostUsd(model, usage);
    await env.DB.prepare(
      `INSERT INTO usage_events
         (account_id, request_id, model, input_tokens, output_tokens,
          cache_read_tokens, cache_write_tokens, cost_usd)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
    )
      .bind(
        accountId,
        requestId,
        model,
        usage.input_tokens || 0,
        usage.output_tokens || 0,
        usage.cache_read_tokens || 0,
        usage.cache_write_tokens || 0,
        costUsd,
      )
      .run();
    console.log(`request ${requestId}: metered cost_usd=${costUsd.toFixed(6)}`);
  } catch (err) {
    console.error(`request ${requestId}: metering failed:`, err);
  }
}

// --- Anthropic proxy with translation ---

async function proxyAnthropic(env, ctx, body, modelId, requestId, meter) {
  const anthropicBody = openaiToAnthropicRequest(body);
  anthropicBody.stream = true; // always stream upstream; we shape the response

  const upstream = await fetch(ANTHROPIC_URL, {
    method: "POST",
    headers: {
      "x-api-key": env.ANTHROPIC_API_KEY,
      "anthropic-version": ANTHROPIC_VERSION,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(anthropicBody),
  });

  if (!upstream.ok) {
    const text = await upstream.text().catch(() => "");
    console.error(`request ${requestId}: anthropic upstream ${upstream.status}: ${text.slice(0, 500)}`);
    return errorResponse(upstream.status === 429 ? 429 : 502, "upstream_error", "upstream provider error", {
      request_id: requestId,
    });
  }

  const translator = new AnthropicToOpenAIStreamTranslator({ model: modelId, requestId });
  const parser = new SseParser();
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  const wantStream = Boolean(body.stream);

  if (wantStream) {
    let meterDone;
    const metered = new Promise((resolve) => (meterDone = resolve));
    const transform = new TransformStream({
      transform(chunk, controller) {
        for (const evt of parser.feed(decoder.decode(chunk, { stream: true }))) {
          const out = translator.handleEvent(evt);
          if (out) controller.enqueue(encoder.encode(out));
        }
      },
      flush(controller) {
        for (const evt of parser.flush()) {
          const out = translator.handleEvent(evt);
          if (out) controller.enqueue(encoder.encode(out));
        }
        const tail = translator.finish();
        if (tail) controller.enqueue(encoder.encode(tail));
        meterDone(meter(translator.usage));
      },
    });
    ctx.waitUntil(metered.then((p) => p));
    upstream.body.pipeTo(transform.writable).catch((err) => {
      console.error(`request ${requestId}: anthropic pipe error:`, err);
    });
    return sseResponse(transform.readable, requestId);
  }

  // Non-streaming client: consume the upstream stream, then respond once.
  const { text, toolCalls } = await consumeAnthropicStream(upstream.body, parser, translator, decoder);
  ctx.waitUntil(meter(translator.usage));
  const message = { role: "assistant", content: text || null };
  if (toolCalls.length) message.tool_calls = toolCalls;
  return jsonResponse(
    {
      id: translator.id,
      object: "chat.completion",
      created: translator.created,
      model: modelId,
      choices: [
        { index: 0, message, finish_reason: translator.finishReason || "stop" },
      ],
      usage: {
        prompt_tokens:
          translator.usage.input_tokens +
          translator.usage.cache_read_tokens +
          translator.usage.cache_write_tokens,
        completion_tokens: translator.usage.output_tokens,
        total_tokens:
          translator.usage.input_tokens +
          translator.usage.cache_read_tokens +
          translator.usage.cache_write_tokens +
          translator.usage.output_tokens,
      },
    },
    200,
    { "X-Request-Id": requestId },
  );
}

async function consumeAnthropicStream(stream, parser, translator, decoder) {
  let text = "";
  const toolCallsByIndex = new Map();
  const handle = (evt) => {
    let parsed;
    try {
      parsed = JSON.parse(evt.data);
    } catch {
      return;
    }
    const type = evt.event || parsed.type;
    if (type === "content_block_start" && parsed.content_block?.type === "tool_use") {
      toolCallsByIndex.set(parsed.index, {
        id: parsed.content_block.id,
        type: "function",
        function: { name: parsed.content_block.name, arguments: "" },
      });
    } else if (type === "content_block_delta") {
      const d = parsed.delta || {};
      if (d.type === "text_delta") text += d.text;
      else if (d.type === "input_json_delta") {
        const tc = toolCallsByIndex.get(parsed.index);
        if (tc) tc.function.arguments += d.partial_json;
      }
    }
    // Let the translator track usage/finish_reason regardless.
    translator.handleEvent(evt);
  };
  const reader = stream.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    for (const evt of parser.feed(decoder.decode(value, { stream: true }))) handle(evt);
  }
  for (const evt of parser.flush()) handle(evt);
  return { text, toolCalls: [...toolCallsByIndex.values()] };
}

// --- OpenAI passthrough proxy ---

async function proxyOpenAI(env, ctx, body, requestId, meter) {
  const upstreamBody = { ...body };
  if (upstreamBody.stream) {
    // Ensure the final chunk carries usage so we can meter from the tail.
    upstreamBody.stream_options = { ...(upstreamBody.stream_options || {}), include_usage: true };
  }

  const upstream = await fetch(OPENAI_URL, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.OPENAI_API_KEY}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(upstreamBody),
  });

  if (!upstream.ok) {
    const text = await upstream.text().catch(() => "");
    console.error(`request ${requestId}: openai upstream ${upstream.status}: ${text.slice(0, 500)}`);
    return errorResponse(upstream.status === 429 ? 429 : 502, "upstream_error", "upstream provider error", {
      request_id: requestId,
    });
  }

  if (!body.stream) {
    const json = await upstream.json();
    const u = json.usage || {};
    const cacheRead = u.prompt_tokens_details?.cached_tokens || 0;
    ctx.waitUntil(
      meter({
        input_tokens: Math.max(0, (u.prompt_tokens || 0) - cacheRead),
        output_tokens: u.completion_tokens || 0,
        cache_read_tokens: cacheRead,
        cache_write_tokens: 0,
      }),
    );
    return jsonResponse(json, 200, { "X-Request-Id": requestId });
  }

  // Streaming: passthrough while sniffing the usage tail. Do not buffer.
  const parser = new SseParser();
  const decoder = new TextDecoder();
  let usage = null;
  let meterDone;
  const metered = new Promise((resolve) => (meterDone = resolve));
  const transform = new TransformStream({
    transform(chunk, controller) {
      controller.enqueue(chunk); // passthrough untouched
      for (const evt of parser.feed(decoder.decode(chunk, { stream: true }))) {
        sniff(evt);
      }
    },
    flush() {
      for (const evt of parser.flush()) sniff(evt);
      meterDone(
        meter(
          usage || {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
          },
        ),
      );
    },
  });
  function sniff(evt) {
    if (!evt.data || evt.data === "[DONE]") return;
    try {
      const parsed = JSON.parse(evt.data);
      const u = usageFromOpenAIChunk(parsed);
      if (u) usage = u;
    } catch {
      // ignore malformed lines
    }
  }
  ctx.waitUntil(metered.then((p) => p));
  upstream.body.pipeTo(transform.writable).catch((err) => {
    console.error(`request ${requestId}: openai pipe error:`, err);
  });
  return sseResponse(transform.readable, requestId);
}

// ---------------------------------------------------------------------------
// Rate limiting: sliding 60s window per key, stored in D1.
//
// v1 limitation: this costs one write + one read per request and is only as
// consistent as D1 (single primary, so it is actually globally consistent,
// but adds latency). If it becomes hot, move to Durable Objects or accept
// per-isolate in-memory counting.
// ---------------------------------------------------------------------------

async function checkRateLimit(env, ctx, keyId, tier) {
  const limit = TIER_RATE_LIMIT_PER_MIN[tier] ?? 60;
  const nowMs = Date.now();
  const windowStart = nowMs - 60_000;
  const row = await env.DB.prepare(
    `SELECT COUNT(*) AS n FROM rate_events WHERE key_id = ? AND at_ms > ?`,
  )
    .bind(keyId, windowStart)
    .first();
  if ((row?.n || 0) >= limit) {
    return errorResponse(429, "rate_limited", `rate limit of ${limit} requests/min exceeded`, {
      retry_after_secs: 15,
    });
  }
  ctx.waitUntil(
    (async () => {
      await env.DB.prepare(`INSERT INTO rate_events (key_id, at_ms) VALUES (?, ?)`)
        .bind(keyId, nowMs)
        .run();
      // Lazy prune: drop entries older than 5 minutes, occasionally.
      if (nowMs % 20 === 0) {
        await env.DB.prepare(`DELETE FROM rate_events WHERE at_ms < ?`)
          .bind(nowMs - 300_000)
          .run();
      }
    })().catch((err) => console.error("rate limit write failed:", err)),
  );
  return null;
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

async function handleHealth(env) {
  let dbSize = null;
  try {
    const result = await env.DB.prepare("SELECT 1").run();
    dbSize = result?.meta?.size_after ?? null;
  } catch (err) {
    return jsonResponse({ ok: false, error: String(err) }, 500);
  }
  return jsonResponse({ ok: true, db_size_bytes: dbSize });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function readJson(request) {
  try {
    return await request.json();
  } catch {
    return null;
  }
}

function round6(n) {
  return Math.round(n * 1e6) / 1e6;
}

function jsonResponse(data, status = 200, extraHeaders = {}) {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json", ...extraHeaders },
  });
}

function errorResponse(status, code, message, extra = {}, extraHeaders = {}) {
  return jsonResponse({ error: { code, message, ...extra } }, status, extraHeaders);
}

function sseResponse(readable, requestId) {
  return new Response(readable, {
    status: 200,
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
      "X-Request-Id": requestId,
    },
  });
}

function htmlResponse(title, message, status = 200) {
  const html = `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>${title} — jcode</title>
  <style>
    body { font-family: system-ui, sans-serif; display: flex; align-items: center;
           justify-content: center; min-height: 100vh; margin: 0; background: #0b0e14; color: #e6e6e6; }
    .card { text-align: center; padding: 2.5rem 3rem; border: 1px solid #2a2f3a;
            border-radius: 12px; background: #11151f; max-width: 28rem; }
    h1 { font-size: 1.4rem; margin: 0 0 0.75rem; }
    p { color: #9aa4b2; margin: 0; line-height: 1.5; }
  </style>
</head>
<body>
  <div class="card">
    <h1>${title}</h1>
    <p>${message}</p>
  </div>
</body>
</html>`;
  return new Response(html, {
    status,
    headers: { "Content-Type": "text/html; charset=utf-8" },
  });
}
