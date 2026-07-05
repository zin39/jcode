-- Schema for the jcode subscription D1 database (accounts, keys, auth, usage).

CREATE TABLE IF NOT EXISTS accounts (
    account_id TEXT PRIMARY KEY,               -- uuid
    email TEXT NOT NULL UNIQUE,
    tier TEXT NOT NULL DEFAULT 'none',         -- 'none' | 'plus' | 'flagship'
    status TEXT NOT NULL DEFAULT 'active',     -- 'active' | 'past_due' | 'canceled'
    stripe_customer_id TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_accounts_stripe_customer
    ON accounts(stripe_customer_id);

-- API keys. Only the SHA-256 hash of the key is stored; the plaintext
-- jck_live_<40 hex> key is shown once at issuance.
CREATE TABLE IF NOT EXISTS keys (
    key_id TEXT PRIMARY KEY,                   -- uuid
    account_id TEXT NOT NULL,
    key_hash TEXT NOT NULL UNIQUE,             -- sha256 hex of full key string
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    revoked_at TEXT,
    FOREIGN KEY (account_id) REFERENCES accounts(account_id)
);

CREATE INDEX IF NOT EXISTS idx_keys_account ON keys(account_id);

-- Pending device-code auth flows (CLI magic-link login).
CREATE TABLE IF NOT EXISTS device_auth (
    device_code TEXT PRIMARY KEY,              -- uuid, held by the CLI
    verify_token TEXT NOT NULL UNIQUE,         -- uuid, embedded in the email link
    email TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',    -- 'pending' | 'approved' | 'consumed'
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    expires_at TEXT NOT NULL,
    approved_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_device_auth_expires ON device_auth(expires_at);

-- One row per proxied completion request, written via ctx.waitUntil after the
-- stream finishes. Budget window = current UTC calendar month.
CREATE TABLE IF NOT EXISTS usage_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (account_id) REFERENCES accounts(account_id)
);

CREATE INDEX IF NOT EXISTS idx_usage_account_created
    ON usage_events(account_id, created_at);
CREATE INDEX IF NOT EXISTS idx_usage_request ON usage_events(request_id);

-- Sliding-window rate limiting (v1: one row per request, pruned lazily).
CREATE TABLE IF NOT EXISTS rate_events (
    key_id TEXT NOT NULL,
    at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_rate_key_at ON rate_events(key_id, at_ms);

-- Waitlist signups from the public pricing page (POST /v1/waitlist).
CREATE TABLE IF NOT EXISTS waitlist (
    email TEXT PRIMARY KEY,
    tier TEXT NOT NULL,                        -- 'plus' | 'flagship'
    note TEXT,                                 -- optional, <= 500 chars
    referrer TEXT,                             -- Referer header at signup
    status TEXT NOT NULL DEFAULT 'pending',    -- 'pending' | 'invited' | 'converted'
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_waitlist_tier_status ON waitlist(tier, status);
