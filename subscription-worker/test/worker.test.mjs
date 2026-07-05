// Unit tests for the subscription worker's pure logic (src/lib.js) plus a
// fetch-mocked translation round-trip. Run with: npm test (node --test).
import test from "node:test";
import assert from "node:assert/strict";

import {
  API_KEY_RE,
  AnthropicToOpenAIStreamTranslator,
  MODELS,
  PRICES_PER_MTOK,
  SseParser,
  TIER_BUDGET_USD,
  budgetWindow,
  computeCostUsd,
  generateApiKey,
  hashApiKey,
  hmacSha256Hex,
  openaiToAnthropicRequest,
  parseStripeSignatureHeader,
  sqliteUtc,
  timingSafeEqualHex,
  usageFromOpenAIChunk,
  verifyStripeSignature,
  isValidEmail,
  validateWaitlistSignup,
  WAITLIST_NOTE_MAX_CHARS,
  WAITLIST_TIERS,
} from "../src/lib.js";

// ---------------------------------------------------------------------------
// Email + waitlist validation
// ---------------------------------------------------------------------------

test("isValidEmail accepts normal addresses and rejects junk", () => {
  assert.equal(isValidEmail("user@example.com"), true);
  assert.equal(isValidEmail("first.last+tag@sub.example.co"), true);
  assert.equal(isValidEmail(""), false);
  assert.equal(isValidEmail("no-at-sign"), false);
  assert.equal(isValidEmail("no-domain@"), false);
  assert.equal(isValidEmail("@no-local.com"), false);
  assert.equal(isValidEmail("no-tld@example"), false);
  assert.equal(isValidEmail("spa ce@example.com"), false);
  assert.equal(isValidEmail(null), false);
  // Length cap: 254 chars max.
  const long = "a".repeat(250) + "@b.co";
  assert.equal(isValidEmail(long), false);
});

test("validateWaitlistSignup normalizes valid signups", () => {
  assert.deepEqual(validateWaitlistSignup({ email: " User@Example.COM ", tier: "plus" }), {
    email: "user@example.com",
    tier: "plus",
    note: null,
  });
  assert.deepEqual(
    validateWaitlistSignup({ email: "a@b.co", tier: "flagship", note: "  team of 5  " }),
    { email: "a@b.co", tier: "flagship", note: "team of 5" },
  );
  // Blank note treated as absent.
  assert.equal(validateWaitlistSignup({ email: "a@b.co", tier: "plus", note: "  " }).note, null);
});

test("validateWaitlistSignup rejects bad email, tier, and long notes", () => {
  assert.equal(validateWaitlistSignup({ email: "junk", tier: "plus" }).error.code, "invalid_email");
  assert.equal(validateWaitlistSignup({ tier: "plus" }).error.code, "invalid_email");
  assert.equal(validateWaitlistSignup({ email: "a@b.co", tier: "pro" }).error.code, "invalid_tier");
  assert.equal(validateWaitlistSignup({ email: "a@b.co" }).error.code, "invalid_tier");
  assert.equal(validateWaitlistSignup(null).error.code, "invalid_email");
  const note = "x".repeat(WAITLIST_NOTE_MAX_CHARS + 1);
  assert.equal(
    validateWaitlistSignup({ email: "a@b.co", tier: "plus", note }).error.code,
    "note_too_long",
  );
  // Exactly at the cap is fine.
  assert.equal(
    validateWaitlistSignup({ email: "a@b.co", tier: "plus", note: "x".repeat(WAITLIST_NOTE_MAX_CHARS) }).note.length,
    WAITLIST_NOTE_MAX_CHARS,
  );
});

test("WAITLIST_TIERS matches the sellable tiers", () => {
  assert.deepEqual(WAITLIST_TIERS, ["plus", "flagship"]);
});

// ---------------------------------------------------------------------------
// API keys
// ---------------------------------------------------------------------------

test("generateApiKey produces jck_live_<40 hex> format", () => {
  const key = generateApiKey();
  assert.match(key, API_KEY_RE);
  // Deterministic from bytes:
  const fixed = generateApiKey(new Uint8Array(20).fill(0xab));
  assert.equal(fixed, "jck_live_" + "ab".repeat(20));
});

test("hashApiKey is SHA-256 hex of the full key string", async () => {
  // Known vector: sha256("jck_live_" + "00"*20)
  const key = "jck_live_" + "00".repeat(20);
  const hash = await hashApiKey(key);
  assert.match(hash, /^[0-9a-f]{64}$/);
  // Stable and distinct for distinct keys.
  assert.equal(hash, await hashApiKey(key));
  assert.notEqual(hash, await hashApiKey("jck_live_" + "01".repeat(20)));
});

test("timingSafeEqualHex compares correctly", () => {
  assert.equal(timingSafeEqualHex("abcd", "abcd"), true);
  assert.equal(timingSafeEqualHex("abcd", "abce"), false);
  assert.equal(timingSafeEqualHex("abcd", "abc"), false);
  assert.equal(timingSafeEqualHex(null, "abcd"), false);
});

// ---------------------------------------------------------------------------
// Stripe signature verification
// ---------------------------------------------------------------------------

test("parseStripeSignatureHeader extracts t and multiple v1", () => {
  const parsed = parseStripeSignatureHeader("t=12345,v1=aaa,v0=zzz,v1=bbb");
  assert.equal(parsed.t, "12345");
  assert.deepEqual(parsed.v1, ["aaa", "bbb"]);
});

test("verifyStripeSignature accepts a valid signature", async () => {
  const secret = "whsec_test_secret";
  const body = '{"id":"evt_1","type":"checkout.session.completed"}';
  const ts = 1_700_000_000;
  const sig = await hmacSha256Hex(secret, `${ts}.${body}`);
  const header = `t=${ts},v1=${sig}`;
  assert.equal(
    await verifyStripeSignature(body, header, secret, { nowSecs: ts + 10 }),
    true,
  );
});

test("verifyStripeSignature rejects bad signature, stale timestamp, missing parts", async () => {
  const secret = "whsec_test_secret";
  const body = '{"id":"evt_1"}';
  const ts = 1_700_000_000;
  const sig = await hmacSha256Hex(secret, `${ts}.${body}`);

  // Wrong signature
  assert.equal(
    await verifyStripeSignature(body, `t=${ts},v1=${"0".repeat(64)}`, secret, { nowSecs: ts }),
    false,
  );
  // Tampered body
  assert.equal(
    await verifyStripeSignature(body + " ", `t=${ts},v1=${sig}`, secret, { nowSecs: ts }),
    false,
  );
  // Stale timestamp (> 300s tolerance)
  assert.equal(
    await verifyStripeSignature(body, `t=${ts},v1=${sig}`, secret, { nowSecs: ts + 301 }),
    false,
  );
  // Missing header
  assert.equal(await verifyStripeSignature(body, null, secret, { nowSecs: ts }), false);
  // Wrong secret
  assert.equal(
    await verifyStripeSignature(body, `t=${ts},v1=${sig}`, "whsec_other", { nowSecs: ts }),
    false,
  );
});

test("verifyStripeSignature accepts when any v1 matches (key rolling)", async () => {
  const secret = "whsec_test_secret";
  const body = "{}";
  const ts = 1_700_000_000;
  const sig = await hmacSha256Hex(secret, `${ts}.${body}`);
  const header = `t=${ts},v1=${"f".repeat(64)},v1=${sig}`;
  assert.equal(await verifyStripeSignature(body, header, secret, { nowSecs: ts }), true);
});

// ---------------------------------------------------------------------------
// Cost computation
// ---------------------------------------------------------------------------

test("computeCostUsd uses the price table per MTok", () => {
  const usage = {
    input_tokens: 1_000_000,
    output_tokens: 500_000,
    cache_read_tokens: 2_000_000,
    cache_write_tokens: 100_000,
  };
  const p = PRICES_PER_MTOK["claude-opus-4-8"];
  const expected =
    p.input + 0.5 * p.output + 2 * p.cache_read + 0.1 * p.cache_write;
  assert.ok(Math.abs(computeCostUsd("claude-opus-4-8", usage) - expected) < 1e-9);
});

test("computeCostUsd handles missing fields and unknown models", () => {
  assert.equal(computeCostUsd("gpt-5.5", { input_tokens: 0 }), 0);
  assert.equal(computeCostUsd("not-a-model", { input_tokens: 1_000_000 }), 0);
  // 1000 in / 1000 out on gpt-5.5: (1000*1.25 + 1000*10)/1e6
  const cost = computeCostUsd("gpt-5.5", { input_tokens: 1000, output_tokens: 1000 });
  assert.ok(Math.abs(cost - 0.01125) < 1e-9);
});

test("every curated model has a price entry", () => {
  for (const model of Object.keys(MODELS)) {
    assert.ok(PRICES_PER_MTOK[model], `missing price for ${model}`);
  }
});

// ---------------------------------------------------------------------------
// Budget window math
// ---------------------------------------------------------------------------

test("budgetWindow returns UTC month boundaries", () => {
  const now = new Date("2026-07-05T07:15:12Z");
  const { start, resetsAt } = budgetWindow(now);
  assert.equal(start.toISOString(), "2026-07-01T00:00:00.000Z");
  assert.equal(resetsAt.toISOString(), "2026-08-01T00:00:00.000Z");
});

test("budgetWindow handles December -> January rollover", () => {
  const { start, resetsAt } = budgetWindow(new Date("2026-12-31T23:59:59Z"));
  assert.equal(start.toISOString(), "2026-12-01T00:00:00.000Z");
  assert.equal(resetsAt.toISOString(), "2027-01-01T00:00:00.000Z");
});

test("budgetWindow first instant of a month belongs to that month", () => {
  const { start, resetsAt } = budgetWindow(new Date("2026-08-01T00:00:00Z"));
  assert.equal(start.toISOString(), "2026-08-01T00:00:00.000Z");
  assert.equal(resetsAt.toISOString(), "2026-09-01T00:00:00.000Z");
});

test("sqliteUtc formats like datetime('now')", () => {
  assert.equal(sqliteUtc(new Date("2026-07-05T07:15:12.999Z")), "2026-07-05 07:15:12");
});

test("tier budgets match the plan", () => {
  assert.equal(TIER_BUDGET_USD.plus, 18.0);
  assert.equal(TIER_BUDGET_USD.flagship, 3000.0);
});

// ---------------------------------------------------------------------------
// OpenAI -> Anthropic request translation
// ---------------------------------------------------------------------------

test("openaiToAnthropicRequest maps system, messages, params", () => {
  const out = openaiToAnthropicRequest({
    model: "claude-opus-4-8",
    max_tokens: 1024,
    temperature: 0.3,
    top_p: 0.9,
    stop: ["END"],
    stream: true,
    messages: [
      { role: "system", content: "Be terse." },
      { role: "user", content: "Hello" },
      { role: "assistant", content: "Hi!" },
      { role: "user", content: [{ type: "text", text: "part1" }, { type: "text", text: "part2" }] },
    ],
  });
  assert.equal(out.model, "claude-opus-4-8");
  assert.equal(out.max_tokens, 1024);
  assert.equal(out.system, "Be terse.");
  assert.equal(out.temperature, 0.3);
  assert.equal(out.top_p, 0.9);
  assert.deepEqual(out.stop_sequences, ["END"]);
  assert.equal(out.stream, true);
  assert.deepEqual(out.messages, [
    { role: "user", content: "Hello" },
    { role: "assistant", content: "Hi!" },
    { role: "user", content: "part1\npart2" },
  ]);
});

test("openaiToAnthropicRequest translates tools and tool messages", () => {
  const out = openaiToAnthropicRequest({
    model: "claude-fable-5",
    messages: [
      { role: "user", content: "What is the weather?" },
      {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: "call_1",
            type: "function",
            function: { name: "get_weather", arguments: '{"city":"SF"}' },
          },
        ],
      },
      { role: "tool", tool_call_id: "call_1", content: "72F sunny" },
    ],
    tools: [
      {
        type: "function",
        function: {
          name: "get_weather",
          description: "Get weather",
          parameters: { type: "object", properties: { city: { type: "string" } } },
        },
      },
    ],
    tool_choice: "auto",
  });

  assert.deepEqual(out.tools, [
    {
      name: "get_weather",
      description: "Get weather",
      input_schema: { type: "object", properties: { city: { type: "string" } } },
    },
  ]);
  assert.deepEqual(out.tool_choice, { type: "auto" });
  assert.deepEqual(out.messages[1], {
    role: "assistant",
    content: [
      { type: "tool_use", id: "call_1", name: "get_weather", input: { city: "SF" } },
    ],
  });
  assert.deepEqual(out.messages[2], {
    role: "user",
    content: [{ type: "tool_result", tool_use_id: "call_1", content: "72F sunny" }],
  });
});

test("openaiToAnthropicRequest defaults max_tokens and handles developer role", () => {
  const out = openaiToAnthropicRequest({
    model: "claude-opus-4-8",
    messages: [
      { role: "developer", content: "dev instructions" },
      { role: "user", content: "hi" },
    ],
  });
  assert.equal(out.max_tokens, 4096);
  assert.equal(out.system, "dev instructions");
});

// ---------------------------------------------------------------------------
// SSE parsing
// ---------------------------------------------------------------------------

test("SseParser handles events split across chunks", () => {
  const parser = new SseParser();
  let events = parser.feed("event: message_start\ndata: {\"a\":");
  assert.equal(events.length, 0);
  events = parser.feed("1}\n\nevent: ping\ndata: {}\n\n");
  assert.equal(events.length, 2);
  assert.equal(events[0].event, "message_start");
  assert.equal(events[0].data, '{"a":1}');
  assert.equal(events[1].event, "ping");
});

test("SseParser flush returns trailing partial event", () => {
  const parser = new SseParser();
  parser.feed("data: tail-no-blank-line");
  const events = parser.flush();
  assert.equal(events.length, 1);
  assert.equal(events[0].data, "tail-no-blank-line");
});

// ---------------------------------------------------------------------------
// Anthropic -> OpenAI stream translation
// ---------------------------------------------------------------------------

function anthropicSse(events) {
  return events
    .map(({ event, data }) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`)
    .join("");
}

const SAMPLE_ANTHROPIC_STREAM = [
  {
    event: "message_start",
    data: {
      type: "message_start",
      message: {
        id: "msg_1",
        usage: { input_tokens: 100, cache_read_input_tokens: 20, cache_creation_input_tokens: 5 },
      },
    },
  },
  { event: "content_block_start", data: { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } } },
  { event: "content_block_delta", data: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "Hello" } } },
  { event: "content_block_delta", data: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: " world" } } },
  { event: "content_block_stop", data: { type: "content_block_stop", index: 0 } },
  { event: "message_delta", data: { type: "message_delta", delta: { stop_reason: "end_turn" }, usage: { output_tokens: 42 } } },
  { event: "message_stop", data: { type: "message_stop" } },
];

test("AnthropicToOpenAIStreamTranslator emits OpenAI chunks and accumulates usage", () => {
  const tr = new AnthropicToOpenAIStreamTranslator({ model: "claude-opus-4-8", requestId: "req1", created: 123 });
  const parser = new SseParser();
  let out = "";
  for (const evt of parser.feed(anthropicSse(SAMPLE_ANTHROPIC_STREAM))) {
    out += tr.handleEvent(evt);
  }
  out += tr.finish();

  const lines = out.split("\n\n").filter(Boolean);
  assert.equal(lines.at(-1), "data: [DONE]");

  const chunks = lines
    .slice(0, -1)
    .map((l) => JSON.parse(l.replace(/^data: /, "")));
  // First chunk: role
  assert.equal(chunks[0].object, "chat.completion.chunk");
  assert.equal(chunks[0].id, "chatcmpl-req1");
  assert.equal(chunks[0].model, "claude-opus-4-8");
  assert.deepEqual(chunks[0].choices[0].delta, { role: "assistant", content: "" });
  // Text deltas
  const text = chunks
    .map((c) => c.choices[0].delta.content || "")
    .join("");
  assert.equal(text, "Hello world");
  // Final chunk: finish_reason + usage
  const final = chunks.at(-1);
  assert.equal(final.choices[0].finish_reason, "stop");
  assert.equal(final.usage.completion_tokens, 42);
  assert.equal(final.usage.prompt_tokens, 125); // 100 + 20 + 5

  // Accumulated usage for metering
  assert.deepEqual(tr.usage, {
    input_tokens: 100,
    output_tokens: 42,
    cache_read_tokens: 20,
    cache_write_tokens: 5,
  });
});

test("translator maps tool_use blocks to OpenAI tool_calls deltas", () => {
  const stream = [
    { event: "message_start", data: { type: "message_start", message: { usage: { input_tokens: 10 } } } },
    {
      event: "content_block_start",
      data: {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "toolu_1", name: "get_weather" },
      },
    },
    {
      event: "content_block_delta",
      data: { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: '{"city":' } },
    },
    {
      event: "content_block_delta",
      data: { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: '"SF"}' } },
    },
    { event: "message_delta", data: { type: "message_delta", delta: { stop_reason: "tool_use" }, usage: { output_tokens: 7 } } },
    { event: "message_stop", data: { type: "message_stop" } },
  ];
  const tr = new AnthropicToOpenAIStreamTranslator({ model: "claude-fable-5", requestId: "req2" });
  const parser = new SseParser();
  let out = "";
  for (const evt of parser.feed(anthropicSse(stream))) out += tr.handleEvent(evt);
  out += tr.finish();

  const chunks = out
    .split("\n\n")
    .filter((l) => l.startsWith("data: ") && !l.includes("[DONE]"))
    .map((l) => JSON.parse(l.slice(6)));

  const start = chunks.find((c) => c.choices[0].delta.tool_calls?.[0]?.id);
  assert.equal(start.choices[0].delta.tool_calls[0].id, "toolu_1");
  assert.equal(start.choices[0].delta.tool_calls[0].function.name, "get_weather");

  const args = chunks
    .flatMap((c) => c.choices[0].delta.tool_calls || [])
    .map((tc) => tc.function?.arguments || "")
    .join("");
  assert.equal(args, '{"city":"SF"}');

  const final = chunks.at(-1);
  assert.equal(final.choices[0].finish_reason, "tool_calls");
});

test("translator finish() is idempotent and emits exactly one [DONE]", () => {
  const tr = new AnthropicToOpenAIStreamTranslator({ model: "m", requestId: "r" });
  const out = tr.finish() + tr.finish();
  const doneCount = out.split("data: [DONE]").length - 1;
  assert.equal(doneCount, 1);
});

// ---------------------------------------------------------------------------
// OpenAI usage tail extraction
// ---------------------------------------------------------------------------

test("usageFromOpenAIChunk normalizes usage with cached tokens", () => {
  const usage = usageFromOpenAIChunk({
    usage: {
      prompt_tokens: 100,
      completion_tokens: 30,
      total_tokens: 130,
      prompt_tokens_details: { cached_tokens: 40 },
    },
  });
  assert.deepEqual(usage, {
    input_tokens: 60,
    output_tokens: 30,
    cache_read_tokens: 40,
    cache_write_tokens: 0,
  });
  assert.equal(usageFromOpenAIChunk({ choices: [] }), null);
  assert.equal(usageFromOpenAIChunk(null), null);
});

// ---------------------------------------------------------------------------
// Fetch-mocked end-to-end translation through the worker's Anthropic proxy
// ---------------------------------------------------------------------------

import worker from "../src/worker.js";

function makeDb(rows = {}) {
  const executed = [];
  return {
    executed,
    prepare(sql) {
      return {
        bind(...values) {
          return {
            async run() {
              executed.push({ sql, values });
              return { meta: { changes: 1, size_after: 4096 } };
            },
            async first() {
              executed.push({ sql, values });
              if (/FROM keys k JOIN accounts/.test(sql)) return rows.auth ?? null;
              if (/SUM\(cost_usd\)/.test(sql)) return { used: rows.usedUsd ?? 0 };
              if (/COUNT\(\*\) AS n FROM rate_events/.test(sql)) return { n: rows.rateCount ?? 0 };
              return null;
            },
            async all() {
              executed.push({ sql, values });
              return { results: [] };
            },
          };
        },
      };
    },
  };
}

function makeCtx() {
  const promises = [];
  return {
    promises,
    waitUntil(p) {
      promises.push(p);
    },
    async settle() {
      await Promise.all(promises);
    },
  };
}

const TEST_KEY = "jck_live_" + "ab".repeat(20);

function chatRequest(body) {
  return new Request("https://api.example/v1/chat/completions", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${TEST_KEY}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(body),
  });
}

const AUTH_ROW = {
  key_id: "key1",
  account_id: "acct1",
  email: "u@example.com",
  tier: "flagship",
  status: "active",
};

test("chat/completions proxies claude-* to Anthropic with stream translation and meters usage", async (t) => {
  const originalFetch = globalThis.fetch;
  let upstreamRequest = null;
  globalThis.fetch = async (url, init) => {
    upstreamRequest = { url: String(url), init };
    const body = anthropicSse(SAMPLE_ANTHROPIC_STREAM);
    return new Response(body, {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    });
  };
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const db = makeDb({ auth: AUTH_ROW });
  const ctx = makeCtx();
  const env = { DB: db, ANTHROPIC_API_KEY: "sk-ant-test" };

  const res = await worker.fetch(
    chatRequest({
      model: "claude-opus-4-8",
      stream: true,
      messages: [{ role: "user", content: "Hi" }],
    }),
    env,
    ctx,
  );

  assert.equal(res.status, 200);
  assert.equal(res.headers.get("Content-Type"), "text/event-stream");
  assert.match(res.headers.get("X-Request-Id"), /^[0-9a-f-]{36}$/);

  // Upstream got a translated Anthropic request.
  assert.equal(upstreamRequest.url, "https://api.anthropic.com/v1/messages");
  assert.equal(upstreamRequest.init.headers["x-api-key"], "sk-ant-test");
  const sentBody = JSON.parse(upstreamRequest.init.body);
  assert.equal(sentBody.stream, true);
  assert.deepEqual(sentBody.messages, [{ role: "user", content: "Hi" }]);

  // Response stream is OpenAI chunk format ending with [DONE].
  const text = await res.text();
  assert.match(text, /"object":"chat\.completion\.chunk"/);
  assert.match(text, /Hello/);
  assert.ok(text.trimEnd().endsWith("data: [DONE]"));

  // Metering was recorded via ctx.waitUntil.
  await ctx.settle();
  const usageInsert = db.executed.find((e) => /INSERT INTO usage_events/.test(e.sql));
  assert.ok(usageInsert, "usage_events insert missing");
  const [, , model, inTok, outTok, cacheRead, cacheWrite, cost] = usageInsert.values;
  assert.equal(model, "claude-opus-4-8");
  assert.equal(inTok, 100);
  assert.equal(outTok, 42);
  assert.equal(cacheRead, 20);
  assert.equal(cacheWrite, 5);
  assert.ok(Math.abs(cost - computeCostUsd("claude-opus-4-8", tr(100, 42, 20, 5))) < 1e-9);

  function tr(input_tokens, output_tokens, cache_read_tokens, cache_write_tokens) {
    return { input_tokens, output_tokens, cache_read_tokens, cache_write_tokens };
  }
});

test("chat/completions enforces tier model access", async () => {
  const db = makeDb({ auth: { ...AUTH_ROW, tier: "plus" } });
  const res = await worker.fetch(
    chatRequest({ model: "gpt-5.6-sol", messages: [{ role: "user", content: "hi" }] }),
    { DB: db },
    makeCtx(),
  );
  assert.equal(res.status, 403);
  const json = await res.json();
  assert.equal(json.error.code, "model_not_allowed");
});

test("chat/completions returns 402 when budget exhausted", async () => {
  const db = makeDb({ auth: { ...AUTH_ROW, tier: "plus" }, usedUsd: 18.0 });
  const res = await worker.fetch(
    chatRequest({ model: "gpt-5.5", messages: [{ role: "user", content: "hi" }] }),
    { DB: db },
    makeCtx(),
  );
  assert.equal(res.status, 402);
  const json = await res.json();
  assert.equal(json.error.code, "budget_exhausted");
  assert.equal(json.error.budget_usd, 18);
  assert.ok(json.error.resets_at);
});

test("chat/completions returns 429 when rate limited", async () => {
  const db = makeDb({ auth: { ...AUTH_ROW, tier: "plus" }, rateCount: 60 });
  const res = await worker.fetch(
    chatRequest({ model: "gpt-5.5", messages: [{ role: "user", content: "hi" }] }),
    { DB: db },
    makeCtx(),
  );
  assert.equal(res.status, 429);
  const json = await res.json();
  assert.equal(json.error.code, "rate_limited");
});

test("chat/completions rejects bad bearer tokens", async () => {
  const db = makeDb({});
  const res = await worker.fetch(
    new Request("https://api.example/v1/chat/completions", {
      method: "POST",
      headers: { Authorization: "Bearer not-a-key" },
      body: "{}",
    }),
    { DB: db },
    makeCtx(),
  );
  assert.equal(res.status, 401);
});

test("gpt-* streaming passthrough injects include_usage and meters from tail", async (t) => {
  const originalFetch = globalThis.fetch;
  let upstreamRequest = null;
  globalThis.fetch = async (url, init) => {
    upstreamRequest = { url: String(url), init };
    const chunks = [
      { id: "c1", object: "chat.completion.chunk", choices: [{ index: 0, delta: { content: "hey" }, finish_reason: null }] },
      { id: "c1", object: "chat.completion.chunk", choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
      {
        id: "c1",
        object: "chat.completion.chunk",
        choices: [],
        usage: { prompt_tokens: 50, completion_tokens: 9, total_tokens: 59, prompt_tokens_details: { cached_tokens: 10 } },
      },
    ];
    const body = chunks.map((c) => `data: ${JSON.stringify(c)}\n\n`).join("") + "data: [DONE]\n\n";
    return new Response(body, { status: 200, headers: { "Content-Type": "text/event-stream" } });
  };
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const db = makeDb({ auth: AUTH_ROW });
  const ctx = makeCtx();
  const res = await worker.fetch(
    chatRequest({ model: "gpt-5.5", stream: true, messages: [{ role: "user", content: "hi" }] }),
    { DB: db, OPENAI_API_KEY: "sk-test" },
    ctx,
  );

  assert.equal(res.status, 200);
  assert.equal(upstreamRequest.url, "https://api.openai.com/v1/chat/completions");
  const sent = JSON.parse(upstreamRequest.init.body);
  assert.deepEqual(sent.stream_options, { include_usage: true });

  // Passthrough is byte-identical upstream content.
  const text = await res.text();
  assert.match(text, /"content":"hey"/);
  assert.ok(text.includes("data: [DONE]"));

  await ctx.settle();
  const usageInsert = db.executed.find((e) => /INSERT INTO usage_events/.test(e.sql));
  assert.ok(usageInsert);
  const [, , model, inTok, outTok, cacheRead] = usageInsert.values;
  assert.equal(model, "gpt-5.5");
  assert.equal(inTok, 40); // 50 prompt - 10 cached
  assert.equal(outTok, 9);
  assert.equal(cacheRead, 10);
});

// ---------------------------------------------------------------------------
// Waitlist endpoint
// ---------------------------------------------------------------------------

function waitlistRequest(body, { origin = "https://solosystems.dev", referer } = {}) {
  const headers = { "Content-Type": "application/json" };
  if (origin) headers.Origin = origin;
  if (referer) headers.Referer = referer;
  return new Request("https://api.example/v1/waitlist", {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
}

test("POST /v1/waitlist upserts and notifies via Resend without blocking", async (t) => {
  const originalFetch = globalThis.fetch;
  const sent = [];
  globalThis.fetch = async (url, init) => {
    sent.push({ url: String(url), body: JSON.parse(init.body) });
    return new Response("{}", { status: 200 });
  };
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const db = makeDb({});
  const ctx = makeCtx();
  const res = await worker.fetch(
    waitlistRequest(
      { email: "Fan@Example.com", tier: "flagship", note: "team of 5" },
      { referer: "https://solosystems.dev/pricing" },
    ),
    { DB: db, RESEND_API_KEY: "re_test" },
    ctx,
  );

  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { ok: true });
  assert.equal(res.headers.get("Access-Control-Allow-Origin"), "https://solosystems.dev");

  const insert = db.executed.find((e) => /INSERT INTO waitlist/.test(e.sql));
  assert.ok(insert, "waitlist insert missing");
  assert.deepEqual(insert.values, [
    "fan@example.com",
    "flagship",
    "team of 5",
    "https://solosystems.dev/pricing",
  ]);
  assert.match(insert.sql, /ON CONFLICT\(email\)/);

  await ctx.settle();
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, "https://api.resend.com/emails");
  assert.deepEqual(sent[0].body.to, ["jeremy@solosystems.dev"]);
  assert.equal(sent[0].body.subject, "jcode waitlist: flagship signup");
});

test("POST /v1/waitlist succeeds even when the notification email fails", async (t) => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response("boom", { status: 500 });
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const db = makeDb({});
  const ctx = makeCtx();
  const res = await worker.fetch(
    waitlistRequest({ email: "a@b.co", tier: "plus" }),
    { DB: db, RESEND_API_KEY: "re_test" },
    ctx,
  );
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { ok: true });
  await ctx.settle(); // must not reject
});

test("POST /v1/waitlist rejects invalid payloads with 400", async () => {
  const db = makeDb({});
  const cases = [
    [{ email: "junk", tier: "plus" }, "invalid_email"],
    [{ email: "a@b.co", tier: "pro" }, "invalid_tier"],
    [{ email: "a@b.co", tier: "plus", note: "x".repeat(501) }, "note_too_long"],
  ];
  for (const [body, code] of cases) {
    const res = await worker.fetch(waitlistRequest(body), { DB: db }, makeCtx());
    assert.equal(res.status, 400);
    const json = await res.json();
    assert.equal(json.error.code, code);
  }
  // Nothing was written.
  assert.equal(db.executed.some((e) => /INSERT INTO waitlist/.test(e.sql)), false);
});

test("waitlist CORS: allowed origins echoed, others get no CORS headers", async (t) => {
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async () => new Response("{}", { status: 200 });
  t.after(() => {
    globalThis.fetch = originalFetch;
  });

  const env = { DB: makeDb({}), RESEND_API_KEY: "re_test" };

  const pages = await worker.fetch(
    waitlistRequest({ email: "a@b.co", tier: "plus" }, { origin: "https://solosystems.pages.dev" }),
    env,
    makeCtx(),
  );
  assert.equal(pages.headers.get("Access-Control-Allow-Origin"), "https://solosystems.pages.dev");

  const evil = await worker.fetch(
    waitlistRequest({ email: "a@b.co", tier: "plus" }, { origin: "https://evil.example" }),
    env,
    makeCtx(),
  );
  assert.equal(evil.status, 200); // same-origin/no-CORS callers still work
  assert.equal(evil.headers.get("Access-Control-Allow-Origin"), null);

  // Preflight
  const preflight = await worker.fetch(
    new Request("https://api.example/v1/waitlist", {
      method: "OPTIONS",
      headers: { Origin: "https://solosystems.dev" },
    }),
    env,
    makeCtx(),
  );
  assert.equal(preflight.status, 204);
  assert.equal(preflight.headers.get("Access-Control-Allow-Origin"), "https://solosystems.dev");
  assert.match(preflight.headers.get("Access-Control-Allow-Methods"), /POST/);
});
