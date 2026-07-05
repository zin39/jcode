// Pure logic for the subscription worker: key hashing, Stripe signature
// verification, pricing, budget windows, and OpenAI<->Anthropic translation.
// Everything here is side-effect free (WebCrypto only) so it can be
// unit-tested with plain `node --test`.

const encoder = new TextEncoder();

// ---------------------------------------------------------------------------
// Tiers, budgets, rate limits
// ---------------------------------------------------------------------------

export const TIER_BUDGET_USD = {
  plus: 18.0,
  flagship: 3000.0,
};

export const TIER_RATE_LIMIT_PER_MIN = {
  plus: 60,
  flagship: 300,
};

// Curated model catalog. `tiers` lists which tiers may use the model;
// `provider` picks the upstream.
export const MODELS = {
  "claude-opus-4-8": { provider: "anthropic", tiers: ["plus", "flagship"] },
  "gpt-5.5": { provider: "openai", tiers: ["plus", "flagship"] },
  "claude-fable-5": { provider: "anthropic", tiers: ["flagship"] },
  "gpt-5.6-sol": { provider: "openai", tiers: ["flagship"] },
};

// ---------------------------------------------------------------------------
// Pricing: USD per million tokens (MTok), per model.
// This is THE price table; metering costs come from here and nowhere else.
// ---------------------------------------------------------------------------
export const PRICES_PER_MTOK = {
  "claude-opus-4-8": { input: 15.0, output: 75.0, cache_read: 1.5, cache_write: 18.75 },
  "claude-fable-5": { input: 5.0, output: 25.0, cache_read: 0.5, cache_write: 6.25 },
  "gpt-5.5": { input: 1.25, output: 10.0, cache_read: 0.125, cache_write: 0 },
  "gpt-5.6-sol": { input: 21.0, output: 168.0, cache_read: 2.1, cache_write: 0 },
};

/**
 * Compute request cost in USD from token counts.
 * usage: {input_tokens, output_tokens, cache_read_tokens, cache_write_tokens}
 */
export function computeCostUsd(model, usage) {
  const p = PRICES_PER_MTOK[model];
  if (!p) return 0;
  const m = 1_000_000;
  return (
    ((usage.input_tokens || 0) * p.input +
      (usage.output_tokens || 0) * p.output +
      (usage.cache_read_tokens || 0) * p.cache_read +
      (usage.cache_write_tokens || 0) * p.cache_write) /
    m
  );
}

// ---------------------------------------------------------------------------
// Budget window: current UTC calendar month.
// ---------------------------------------------------------------------------

/**
 * Returns {start, resetsAt} as Date objects for the UTC month containing
 * `now` (a Date). start is inclusive, resetsAt is the next month boundary.
 */
export function budgetWindow(now = new Date()) {
  const start = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1));
  const resetsAt = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1));
  return { start, resetsAt };
}

/** Format a Date as the SQLite `datetime('now')` style string (UTC). */
export function sqliteUtc(date) {
  return date.toISOString().replace("T", " ").slice(0, 19);
}

// ---------------------------------------------------------------------------
// Email validation (shared by auth and waitlist).
// ---------------------------------------------------------------------------

export function isValidEmail(email) {
  return (
    typeof email === "string" &&
    email.length <= 254 &&
    /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)
  );
}

// ---------------------------------------------------------------------------
// Waitlist signup validation. Pure: takes a parsed JSON body, returns either
// {error: {code, message}} or the normalized {email, tier, note}.
// ---------------------------------------------------------------------------

export const WAITLIST_TIERS = ["plus", "flagship"];
export const WAITLIST_NOTE_MAX_CHARS = 500;

export function validateWaitlistSignup(body) {
  const email = String(body?.email || "").trim().toLowerCase();
  if (!isValidEmail(email)) {
    return { error: { code: "invalid_email", message: "a valid email is required" } };
  }
  const tier = String(body?.tier || "");
  if (!WAITLIST_TIERS.includes(tier)) {
    return {
      error: {
        code: "invalid_tier",
        message: `tier must be one of: ${WAITLIST_TIERS.join(", ")}`,
      },
    };
  }
  let note = null;
  if (body?.note != null && String(body.note).trim() !== "") {
    note = String(body.note).trim();
    if (note.length > WAITLIST_NOTE_MAX_CHARS) {
      return {
        error: {
          code: "note_too_long",
          message: `note must be at most ${WAITLIST_NOTE_MAX_CHARS} characters`,
        },
      };
    }
  }
  return { email, tier, note };
}

// ---------------------------------------------------------------------------
// API keys: jck_live_<40 lowercase hex>. Only the SHA-256 hex digest of the
// full key string is stored.
// ---------------------------------------------------------------------------

export const API_KEY_RE = /^jck_live_[0-9a-f]{40}$/;

export function generateApiKey(randomBytes = crypto.getRandomValues(new Uint8Array(20))) {
  return "jck_live_" + bytesToHex(randomBytes);
}

export async function hashApiKey(key) {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(key));
  return bytesToHex(new Uint8Array(digest));
}

export function bytesToHex(bytes) {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

// ---------------------------------------------------------------------------
// Stripe webhook signature verification (no SDK).
// Header: Stripe-Signature: t=<ts>,v1=<hex>[,v1=<hex>...]
// signed_payload = `${t}.${rawBody}`; expected = HMAC-SHA256(secret, signed_payload)
// ---------------------------------------------------------------------------

export function parseStripeSignatureHeader(header) {
  const out = { t: null, v1: [] };
  for (const part of String(header || "").split(",")) {
    const idx = part.indexOf("=");
    if (idx < 0) continue;
    const k = part.slice(0, idx).trim();
    const v = part.slice(idx + 1).trim();
    if (k === "t") out.t = v;
    else if (k === "v1") out.v1.push(v);
  }
  return out;
}

export async function hmacSha256Hex(secret, payload) {
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const sig = await crypto.subtle.sign("HMAC", key, encoder.encode(payload));
  return bytesToHex(new Uint8Array(sig));
}

/**
 * Verify a Stripe webhook. Returns true only if a v1 signature matches and
 * the timestamp is within `toleranceSecs` of `nowSecs`.
 */
export async function verifyStripeSignature(
  rawBody,
  header,
  secret,
  { toleranceSecs = 300, nowSecs = Math.floor(Date.now() / 1000) } = {},
) {
  const parsed = parseStripeSignatureHeader(header);
  if (!parsed.t || parsed.v1.length === 0) return false;
  const ts = Number(parsed.t);
  if (!Number.isFinite(ts) || Math.abs(nowSecs - ts) > toleranceSecs) return false;
  const expected = await hmacSha256Hex(secret, `${parsed.t}.${rawBody}`);
  return parsed.v1.some((sig) => timingSafeEqualHex(sig, expected));
}

export function timingSafeEqualHex(a, b) {
  if (typeof a !== "string" || typeof b !== "string" || a.length !== b.length) {
    return false;
  }
  let diff = 0;
  for (let i = 0; i < a.length; i++) {
    diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  }
  return diff === 0;
}

// ---------------------------------------------------------------------------
// OpenAI -> Anthropic request translation.
// Takes an OpenAI /v1/chat/completions body, returns an Anthropic
// /v1/messages body. Supports system messages, text + multimodal-ish content
// arrays (text parts only), tools, and common sampling params.
// ---------------------------------------------------------------------------

export function openaiToAnthropicRequest(body) {
  const out = {
    model: body.model,
    max_tokens: body.max_tokens ?? body.max_completion_tokens ?? 4096,
    messages: [],
  };

  const systemParts = [];
  for (const msg of body.messages || []) {
    if (msg.role === "system" || msg.role === "developer") {
      systemParts.push(contentToText(msg.content));
      continue;
    }
    if (msg.role === "tool") {
      // OpenAI tool result -> Anthropic tool_result content block on a user turn.
      out.messages.push({
        role: "user",
        content: [
          {
            type: "tool_result",
            tool_use_id: msg.tool_call_id,
            content: contentToText(msg.content),
          },
        ],
      });
      continue;
    }
    if (msg.role === "assistant" && Array.isArray(msg.tool_calls) && msg.tool_calls.length) {
      const content = [];
      const text = contentToText(msg.content);
      if (text) content.push({ type: "text", text });
      for (const tc of msg.tool_calls) {
        content.push({
          type: "tool_use",
          id: tc.id,
          name: tc.function?.name,
          input: safeJsonParse(tc.function?.arguments) ?? {},
        });
      }
      out.messages.push({ role: "assistant", content });
      continue;
    }
    out.messages.push({ role: msg.role, content: contentToText(msg.content) });
  }
  if (systemParts.length) out.system = systemParts.join("\n\n");

  if (typeof body.temperature === "number") out.temperature = body.temperature;
  if (typeof body.top_p === "number") out.top_p = body.top_p;
  if (body.stop) out.stop_sequences = Array.isArray(body.stop) ? body.stop : [body.stop];
  if (body.stream) out.stream = true;

  if (Array.isArray(body.tools) && body.tools.length) {
    out.tools = body.tools
      .filter((t) => t.type === "function" && t.function)
      .map((t) => ({
        name: t.function.name,
        description: t.function.description || "",
        input_schema: t.function.parameters || { type: "object", properties: {} },
      }));
  }
  if (body.tool_choice) {
    if (body.tool_choice === "auto") out.tool_choice = { type: "auto" };
    else if (body.tool_choice === "required") out.tool_choice = { type: "any" };
    else if (body.tool_choice === "none") delete out.tools;
    else if (body.tool_choice.function?.name) {
      out.tool_choice = { type: "tool", name: body.tool_choice.function.name };
    }
  }
  return out;
}

function contentToText(content) {
  if (content == null) return "";
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => (typeof part === "string" ? part : part.text || ""))
      .filter(Boolean)
      .join("\n");
  }
  return String(content);
}

function safeJsonParse(text) {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

export function anthropicStopToOpenAIFinish(stopReason) {
  switch (stopReason) {
    case "end_turn":
    case "stop_sequence":
      return "stop";
    case "max_tokens":
      return "length";
    case "tool_use":
      return "tool_calls";
    default:
      return stopReason || "stop";
  }
}

// ---------------------------------------------------------------------------
// SSE parsing + Anthropic -> OpenAI stream translation.
// ---------------------------------------------------------------------------

/**
 * Incremental SSE event parser. Feed text chunks, get [{event, data}] arrays.
 * Call flush() at end-of-stream for any trailing event without a blank line.
 */
export class SseParser {
  constructor() {
    this.buffer = "";
  }
  feed(text) {
    this.buffer += text;
    const events = [];
    let idx;
    while ((idx = this.buffer.indexOf("\n\n")) !== -1) {
      const raw = this.buffer.slice(0, idx);
      this.buffer = this.buffer.slice(idx + 2);
      const evt = parseSseEvent(raw);
      if (evt) events.push(evt);
    }
    return events;
  }
  flush() {
    const raw = this.buffer;
    this.buffer = "";
    const evt = parseSseEvent(raw);
    return evt ? [evt] : [];
  }
}

function parseSseEvent(raw) {
  let event = null;
  const dataLines = [];
  for (const line of raw.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
  }
  if (!event && dataLines.length === 0) return null;
  return { event, data: dataLines.join("\n") };
}

/**
 * Stateful translator from Anthropic /v1/messages SSE events to OpenAI
 * chat.completion.chunk SSE lines. Also accumulates usage from message_start
 * and message_delta so the caller can meter after the stream ends.
 *
 * Usage:
 *   const tr = new AnthropicToOpenAIStreamTranslator({ model, requestId });
 *   for each upstream SSE event: out += tr.handleEvent(evt) (string of SSE lines)
 *   at end: out += tr.finish(); usage = tr.usage
 */
export class AnthropicToOpenAIStreamTranslator {
  constructor({ model, requestId, created = Math.floor(Date.now() / 1000) } = {}) {
    this.model = model;
    this.id = `chatcmpl-${requestId || crypto.randomUUID()}`;
    this.created = created;
    this.usage = {
      input_tokens: 0,
      output_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
    };
    this.finishReason = null;
    this.toolIndexByBlock = new Map(); // anthropic block index -> openai tool_call index
    this.nextToolIndex = 0;
    this.sentFinish = false;
    this.sentDone = false;
  }

  chunk(delta, finishReason = null, extra = {}) {
    const payload = {
      id: this.id,
      object: "chat.completion.chunk",
      created: this.created,
      model: this.model,
      choices: [{ index: 0, delta, finish_reason: finishReason }],
      ...extra,
    };
    return `data: ${JSON.stringify(payload)}\n\n`;
  }

  /** Translate one parsed Anthropic SSE event; returns SSE text to emit ("" if none). */
  handleEvent({ event, data }) {
    let parsed;
    try {
      parsed = JSON.parse(data);
    } catch {
      return "";
    }
    const type = event || parsed.type;
    switch (type) {
      case "message_start": {
        const u = parsed.message?.usage || {};
        this.usage.input_tokens = u.input_tokens || 0;
        this.usage.cache_read_tokens = u.cache_read_input_tokens || 0;
        this.usage.cache_write_tokens = u.cache_creation_input_tokens || 0;
        return this.chunk({ role: "assistant", content: "" });
      }
      case "content_block_start": {
        const block = parsed.content_block;
        if (block?.type === "tool_use") {
          const toolIndex = this.nextToolIndex++;
          this.toolIndexByBlock.set(parsed.index, toolIndex);
          return this.chunk({
            tool_calls: [
              {
                index: toolIndex,
                id: block.id,
                type: "function",
                function: { name: block.name, arguments: "" },
              },
            ],
          });
        }
        return "";
      }
      case "content_block_delta": {
        const d = parsed.delta || {};
        if (d.type === "text_delta") {
          return this.chunk({ content: d.text });
        }
        if (d.type === "input_json_delta") {
          const toolIndex = this.toolIndexByBlock.get(parsed.index) ?? 0;
          return this.chunk({
            tool_calls: [
              { index: toolIndex, function: { arguments: d.partial_json } },
            ],
          });
        }
        return "";
      }
      case "message_delta": {
        const u = parsed.usage || {};
        if (u.output_tokens != null) this.usage.output_tokens = u.output_tokens;
        if (u.input_tokens != null) this.usage.input_tokens = u.input_tokens;
        if (parsed.delta?.stop_reason) {
          this.finishReason = anthropicStopToOpenAIFinish(parsed.delta.stop_reason);
        }
        return "";
      }
      case "message_stop": {
        return this.emitFinish();
      }
      case "error": {
        // Surface upstream errors in-band; the HTTP status is already sent.
        return `data: ${JSON.stringify({ error: parsed.error || parsed })}\n\n`;
      }
      default:
        return ""; // ping, content_block_stop, etc.
    }
  }

  emitFinish() {
    if (this.sentFinish) return "";
    this.sentFinish = true;
    let out = this.chunk({}, this.finishReason || "stop", {
      usage: {
        prompt_tokens: this.usage.input_tokens + this.usage.cache_read_tokens + this.usage.cache_write_tokens,
        completion_tokens: this.usage.output_tokens,
        total_tokens:
          this.usage.input_tokens +
          this.usage.cache_read_tokens +
          this.usage.cache_write_tokens +
          this.usage.output_tokens,
      },
    });
    out += this.finishDone();
    return out;
  }

  finishDone() {
    if (this.sentDone) return "";
    this.sentDone = true;
    return "data: [DONE]\n\n";
  }

  /** Call at end of upstream stream; emits finish chunk + [DONE] if not yet sent. */
  finish() {
    let out = "";
    if (!this.sentFinish) out += this.emitFinish();
    else out += this.finishDone();
    return out;
  }
}

/**
 * Extract usage from OpenAI chat.completion.chunk SSE data payloads.
 * The final chunk (with stream_options.include_usage) carries `usage`.
 * Returns normalized usage or null if the payload has none.
 */
export function usageFromOpenAIChunk(parsed) {
  const u = parsed?.usage;
  if (!u) return null;
  const cacheRead = u.prompt_tokens_details?.cached_tokens || 0;
  return {
    input_tokens: Math.max(0, (u.prompt_tokens || 0) - cacheRead),
    output_tokens: u.completion_tokens || 0,
    cache_read_tokens: cacheRead,
    cache_write_tokens: 0,
  };
}
