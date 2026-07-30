#!/usr/bin/env python3
"""Report how often the coordinator delegates, and whether it names real tools.

Motivation: `cheap_route` was called in only 19 of 4885 sessions while `swarm`
appeared in 229, despite `agents.auto_delegate = true`. Reading the session
files rather than reasoning about it found the cause: the auto-delegation
directive told every coordinator to call `subagent`, which had been deleted
from the registry, so the model obeyed and got `Unknown tool: subagent` back.

That is fixed, but "did the fix change behaviour?" is a question about live
usage, which cannot be answered from a test. This script answers it by
comparing sessions written before and after the currently-installed binary,
so the baseline is recorded rather than remembered.

Usage:
    python3 scripts/delegation_usage_report.py
    python3 scripts/delegation_usage_report.py --since 2026-07-30
"""

from __future__ import annotations

import argparse
import datetime as dt
import glob
import json
import os

SESSION_DIR = os.path.expanduser("~/.jcode/sessions")
BINARY = os.path.expanduser("~/.jcode/builds/current/jcode")

# Tool names the delegation directive tells the coordinator to call. A name
# appearing in PHANTOM means the prompt is advertising something the registry
# does not serve, which is a prompt bug and not a model failure.
DELEGATION_TOOLS = ("cheap_route", "swarm")
PHANTOM_MARKER = "Unknown tool: "


def tool_calls(session: dict) -> list[str]:
    """Names of every tool the assistant actually invoked."""
    names: list[str] = []
    for message in session.get("messages") or []:
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if isinstance(block, dict) and block.get("type") == "tool_use":
                name = block.get("name")
                if isinstance(name, str):
                    names.append(name)
    return names


def phantom_tools(session: dict) -> set[str]:
    """Tools the model called that the registry rejected as unknown.

    Only counts genuine tool_result errors. Matching the raw file text would
    also match a transcript that merely discusses the error, which is how a
    session investigating this bug looks indistinguishable from one hitting it.
    """
    found: set[str] = set()
    for message in session.get("messages") or []:
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            if not block.get("is_error"):
                continue
            body = json.dumps(block.get("content"))
            marker = body.find(PHANTOM_MARKER)
            if marker != -1:
                rest = body[marker + len(PHANTOM_MARKER) :]
                found.add(rest.split(".")[0].strip().strip('\\"'))
    return found


def summarize(paths: list[str]) -> dict:
    stats = {name: 0 for name in DELEGATION_TOOLS}
    stats["sessions"] = 0
    stats["calls"] = {name: 0 for name in DELEGATION_TOOLS}
    stats["phantom_sessions"] = 0
    stats["phantom_names"] = {}
    for path in paths:
        try:
            session = json.loads(open(path, errors="ignore").read())
        except (json.JSONDecodeError, OSError):
            continue
        stats["sessions"] += 1
        names = tool_calls(session)
        for tool in DELEGATION_TOOLS:
            count = names.count(tool)
            if count:
                stats[tool] += 1
                stats["calls"][tool] += count
        phantoms = phantom_tools(session)
        if phantoms:
            stats["phantom_sessions"] += 1
            for name in phantoms:
                stats["phantom_names"][name] = stats["phantom_names"].get(name, 0) + 1
    return stats


def report(label: str, stats: dict) -> None:
    total = stats["sessions"]
    print(f"{label}: {total} sessions")
    if not total:
        return
    for tool in DELEGATION_TOOLS:
        share = 100.0 * stats[tool] / total
        print(
            f"    {tool:<12} used in {stats[tool]:>5} sessions "
            f"({share:5.1f}%), {stats['calls'][tool]} calls"
        )
    print(f"    phantom-tool sessions: {stats['phantom_sessions']}")
    for name, count in sorted(stats["phantom_names"].items(), key=lambda kv: -kv[1]):
        print(f"      {name}: {count}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--since",
        help="YYYY-MM-DD cutoff; defaults to the installed binary's build time.",
    )
    args = parser.parse_args()

    if args.since:
        cutoff = dt.datetime.strptime(args.since, "%Y-%m-%d").timestamp()
        origin = f"--since {args.since}"
    elif os.path.exists(BINARY):
        cutoff = os.path.getmtime(BINARY)
        origin = f"installed binary built {dt.datetime.fromtimestamp(cutoff):%Y-%m-%d %H:%M}"
    else:
        print(f"no binary at {BINARY}; pass --since")
        return 1

    paths = glob.glob(os.path.join(SESSION_DIR, "*.json"))
    if not paths:
        print(f"no sessions under {SESSION_DIR}")
        return 1

    before = [p for p in paths if os.path.getmtime(p) < cutoff]
    after = [p for p in paths if os.path.getmtime(p) >= cutoff]

    print(f"cutoff: {origin}")
    print()
    report("BEFORE", summarize(before))
    print()
    report("AFTER ", summarize(after))
    print()
    print(
        "A phantom-tool count above zero in AFTER means the system prompt is "
        "advertising a tool the registry does not serve."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
