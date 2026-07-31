#!/usr/bin/env bash
# Reclaim leaked test sandboxes and other stale scratch state.
#
# `ensure_thread_test_home` deliberately leaks a per-thread `~/.jcode` sandbox:
# it must outlive every guard a test creates, so it cannot use a scoped guard.
# That is fine when the leak lands in a self-cleaning temp dir, but
# `scripts/dev_cargo.sh` points TMPDIR at `~/.jcode/scratch` to keep cargo's
# build temp on disk, and nothing sweeps that path. One developer machine had
# accumulated 183,948 leaked homes (~920k files).
#
# New homes are created under `$TMPDIR/jcode-test-homes/` so they can be
# removed in one step. This script clears that root plus any historical homes
# still loose in scratch.
#
# Usage:
#   scripts/clean_scratch.sh            # report only, delete nothing
#   scripts/clean_scratch.sh --apply    # actually delete

set -euo pipefail

apply=0
[[ "${1:-}" == "--apply" ]] && apply=1

jcode_home="${JCODE_HOME:-$HOME/.jcode}"
scratch="$jcode_home/scratch"
contained="${TMPDIR:-/tmp}/jcode-test-homes"

count_dirs() {
  # `find -maxdepth 1` rather than `ls | wc -l` so a huge directory streams
  # instead of materializing every name, and a missing dir is simply zero.
  [[ -d "$1" ]] || { echo 0; return; }
  find "$1" -maxdepth 1 -mindepth 1 -name "${2:-*}" 2>/dev/null | wc -l | tr -d ' '
}

loose=$(count_dirs "$scratch" 'jcode-test-home-*')
held=$(count_dirs "$contained")

echo "Leaked test sandboxes"
echo "  loose in $scratch: $loose"
echo "  under $contained: $held"

if (( loose == 0 && held == 0 )); then
  echo "Nothing to reclaim."
  exit 0
fi

if (( apply == 0 )); then
  echo
  echo "Report only. Re-run with --apply to delete these."
  exit 0
fi

# Delete via find rather than a glob: 180k+ entries overflow ARG_MAX, and the
# glob would fail the whole command instead of removing anything.
if (( loose > 0 )); then
  find "$scratch" -maxdepth 1 -mindepth 1 -name 'jcode-test-home-*' -exec rm -rf {} + 2>/dev/null || true
fi
if (( held > 0 )); then
  rm -rf "$contained" 2>/dev/null || true
fi

echo
echo "Reclaimed. Remaining:"
echo "  loose: $(count_dirs "$scratch" 'jcode-test-home-*')"
echo "  contained: $(count_dirs "$contained")"
