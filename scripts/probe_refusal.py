#!/usr/bin/env python3
"""
Measures whether jcode's swarm tool description triggers Anthropic's
safety classifier (stop_reason='refusal') on a trivial 'hi' message.

This is an honest measurement instrument. It does NOT fabricate results.

Usage:
    python3 scripts/probe_refusal.py [--model MODEL] [--n N] [--variant VARIANT]

Variants:
    bare                 - no system prompt, no tools (control)
    swarm_desc           - one tool named 'swarm' with raw ~/.jcode/swarm-prompt.md
    swarm_desc_sanitized - same but competitor model names + benchmark names redacted

Reports: per-variant summary with refusal count, categories observed, and request IDs.
Never prints the API key or full response bodies.
"""

import argparse
import json
import os
import re
import sys
from pathlib import Path

try:
    import requests
    _USE_REQUESTS = True
except ImportError:
    import urllib.request
    import urllib.error
    _USE_REQUESTS = False


ANTHROPIC_API_URL = "https://api.anthropic.com/v1/messages"
ANTHROPIC_API_VERSION = "2023-06-01"
SWARM_PROMPT_PATH = Path.home() / ".jcode" / "swarm-prompt.md"

# ---- Sanitization helpers ------------------------------------------------

# Patterns to redact from tool descriptions in the sanitized variant.
# These cover the frontier-LLM-sensitive terms: competitor model names and
# benchmark suite names that may trigger the 'frontier_llm' classifier.
_REDACT_PATTERNS = [
    # Company/product model families
    (r'\bGLM[-\s][\d.]+(?:-\w+)?', '[MODEL]'),
    (r'\bglm[-\s][\d.]+(?:-\w+)?', '[model]'),
    (r'\bglm-\*', '[model-*]'),
    (r'\b(deepseek|DeepSeek)[-\s]?v[\d.]+(?:-(?:pro|flash))?', '[MODEL]'),
    (r'\b(deepseek|DeepSeek)[-\s]?v4[-\s]?(?:pro|flash)?', '[MODEL]'),
    (r'\bDeepSeek\b', '[VENDOR]'),
    (r'\bdeepseek\b', '[vendor]'),
    (r'\bKimi[-\s]K[\d.]+(?:-\w+)?', '[MODEL]'),
    (r'\bkimi[-\s]k[\d.]+(?:-\w+)?', '[model]'),
    (r'\bkimi-\*', '[model-*]'),
    (r'\bKimi\b', '[VENDOR]'),
    (r'\bkimi\b', '[vendor]'),
    (r'\bMiniMax[-\s]?M[\d]+', '[MODEL]'),
    (r'\bMiniMax\b', '[VENDOR]'),
    (r'\bQwen[\d.]*(?:[-\s]\w+)*', '[MODEL]'),
    (r'\bqwen[\d.]*(?:[-\s]\w+)*', '[model]'),
    (r'\bGemini[\s][\d.]+(?:\s\w+)?', '[MODEL]'),
    (r'\bgemini[-\s][\d.]+(?:-\w+)?', '[model]'),
    (r'\bgpt-[\d.]+(?:-\w+)?', '[model]'),
    (r'\bGPT-[\d.]+(?:-\w+)?', '[MODEL]'),
    (r'\bclaude[-\s][\w\d.-]+', '[model]'),
    (r'\bClaude[-\s][\w\d.-]+', '[MODEL]'),
    # Benchmark names
    (r'\bSWE-bench\b(?:\s+(?:Pro|Verified))?', '[BENCHMARK]'),
    (r'\bTerminal-Bench\b(?:\s+[\d.]+)?', '[BENCHMARK]'),
    (r'\bAA\s+Intelligence\s+Index\b', '[BENCHMARK]'),
    (r'\bArena\.ai\b', '[ARENA]'),
    (r'\bVals\s+AI\b', '[EVAL_VENDOR]'),
    (r'\bSemgrep\b', '[VENDOR]'),
    # Company names likely to trigger frontier_llm
    (r'\bAnthropic\b', '[AI_CO]'),
    (r'\bOpenAI\b', '[AI_CO]'),
    (r'\bMoonshot\b', '[VENDOR]'),
    (r'\bAlib[a]+ba\b', '[VENDOR]'),
    (r'\bdashscope\b', '[PROVIDER]'),
    (r'\bZ\.AI\b', '[VENDOR]'),
]

_COMPILED = [(re.compile(pat, re.IGNORECASE if pat == pat.lower() else 0), rep)
             for pat, rep in _REDACT_PATTERNS]


def sanitize_description(text: str) -> str:
    """Remove competitor model names and benchmark names from a tool description."""
    result = text
    for pattern, replacement in _COMPILED:
        result = pattern.sub(replacement, result)
    return result


# ---- Variant builders ----------------------------------------------------

def build_swarm_tool(description: str) -> dict:
    return {
        "name": "swarm",
        "description": description,
        "input_schema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "description": "Action to perform."},
            },
            "required": ["action"],
        },
    }


def build_request_body(variant: str, swarm_desc_raw: str, model: str) -> dict:
    base = {
        "model": model,
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "hi"}],
    }
    if variant == "bare":
        pass  # no tools, no system
    elif variant == "swarm_desc":
        base["tools"] = [build_swarm_tool(swarm_desc_raw)]
    elif variant == "swarm_desc_sanitized":
        sanitized = sanitize_description(swarm_desc_raw)
        base["tools"] = [build_swarm_tool(sanitized)]
    else:
        raise ValueError(f"Unknown variant: {variant!r}")
    return base


# ---- HTTP helpers ---------------------------------------------------------

def make_request(api_key: str, body: dict) -> tuple[int, dict, str]:
    """
    Send POST to Anthropic messages API.
    Returns (http_status, response_json_or_empty_dict, request_id).
    """
    headers = {
        "x-api-key": api_key,
        "anthropic-version": ANTHROPIC_API_VERSION,
        "content-type": "application/json",
    }
    payload = json.dumps(body).encode()

    if _USE_REQUESTS:
        resp = requests.post(ANTHROPIC_API_URL, headers=headers, data=payload, timeout=60)
        status = resp.status_code
        req_id = resp.headers.get("request-id", resp.headers.get("x-request-id", ""))
        try:
            data = resp.json()
        except Exception:
            data = {}
        return status, data, req_id
    else:
        req = urllib.request.Request(ANTHROPIC_API_URL, data=payload, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                status = resp.status
                req_id = resp.headers.get("request-id", resp.headers.get("x-request-id", ""))
                raw = resp.read()
        except urllib.error.HTTPError as e:
            status = e.code
            req_id = e.headers.get("request-id", e.headers.get("x-request-id", ""))
            raw = e.read()
        try:
            data = json.loads(raw)
        except Exception:
            data = {}
        return status, data, req_id


# ---- Safe response parsing -----------------------------------------------

def parse_result(status: int, data: dict) -> tuple[str | None, dict | None]:
    """
    Extract (stop_reason, stop_details) from a response dict.
    Returns (None, None) if not present.
    """
    stop_reason = data.get("stop_reason")
    stop_details = data.get("stop_details")  # dict or None
    return stop_reason, stop_details


# ---- Main ----------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Probe Anthropic refusal classifier against jcode tool descriptions."
    )
    parser.add_argument(
        "--model",
        default="claude-sonnet-4-5-20250929",
        help="Anthropic model ID to use (default: claude-sonnet-4-5-20250929)",
    )
    parser.add_argument(
        "--n",
        type=int,
        default=3,
        help="Number of repetitions per variant (default: 3)",
    )
    parser.add_argument(
        "--variant",
        default=None,
        help="Run only this variant (bare|swarm_desc|swarm_desc_sanitized). Default: all three.",
    )
    args = parser.parse_args()

    # --- Validate API key ---
    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        print("ERROR: ANTHROPIC_API_KEY environment variable is not set.", file=sys.stderr)
        print("Set it and re-run. This script does not read tokens from disk.", file=sys.stderr)
        sys.exit(2)

    # --- Load swarm prompt ---
    if not SWARM_PROMPT_PATH.exists():
        print(f"WARNING: {SWARM_PROMPT_PATH} not found; swarm_desc variants will use empty description.")
        swarm_desc_raw = ""
    else:
        swarm_desc_raw = SWARM_PROMPT_PATH.read_text(encoding="utf-8")

    print(f"swarm-prompt.md size: {len(swarm_desc_raw)} bytes")
    print(f"model: {args.model}  n: {args.n}")
    print()

    all_variants = ["bare", "swarm_desc", "swarm_desc_sanitized"]
    variants_to_run = [args.variant] if args.variant else all_variants

    results: dict[str, list[dict]] = {}

    for variant in variants_to_run:
        print(f"--- variant={variant} ---")
        runs = []
        for i in range(1, args.n + 1):
            body = build_request_body(variant, swarm_desc_raw, args.model)
            status, data, req_id = make_request(api_key, body)
            stop_reason, stop_details = parse_result(status, data)

            # Determine if this is a refusal
            is_refusal = (stop_reason == "refusal")

            # Safe excerpt: only stop_reason, stop_details, error type (no full text)
            error_type = data.get("error", {}).get("type") if isinstance(data.get("error"), dict) else None

            run_info = {
                "run": i,
                "http_status": status,
                "stop_reason": stop_reason,
                "stop_details": stop_details,
                "error_type": error_type,
                "request_id": req_id,
                "is_refusal": is_refusal,
            }
            runs.append(run_info)

            # Per-run line
            category = stop_details.get("category") if stop_details else None
            explanation_snippet = ""
            if stop_details and stop_details.get("explanation"):
                explanation_snippet = stop_details["explanation"][:120]
            print(
                f"  run={i} status={status} stop_reason={stop_reason!r} "
                f"category={category!r} req_id={req_id!r}"
            )
            if explanation_snippet:
                print(f"    explanation[0:120]: {explanation_snippet!r}")

        results[variant] = runs

    print()
    print("=== SUMMARY ===")
    for variant, runs in results.items():
        n_refusals = sum(1 for r in runs if r["is_refusal"])
        n_total = len(runs)
        categories = [
            r["stop_details"]["category"]
            for r in runs
            if r["is_refusal"] and r["stop_details"] and r["stop_details"].get("category")
        ]
        print(f"variant={variant} refusals={n_refusals}/{n_total} categories={categories}")

    print()
    print("=== SANITIZED DESC (first 400 chars) ===")
    sanitized = sanitize_description(swarm_desc_raw)
    print(sanitized[:400])
    print("...")


if __name__ == "__main__":
    main()
