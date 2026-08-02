#!/usr/bin/env bash
#
# Purge every previously installed jcode from a Linux host and install one
# explicit build, so no stale version can come back.
#
# Run this ON the VM, not on the Mac.
#
#   scp dist/jcode-linux-x86_64.tar.gz user@vm:/tmp/
#   scp scripts/vm_purge_install.sh    user@vm:/tmp/
#   ssh user@vm 'bash /tmp/vm_purge_install.sh /tmp/jcode-linux-x86_64.tar.gz'
#
# Why this script exists rather than a handful of pasted commands:
#
#   1. A live jcode server keeps running its own binary from memory, and its
#      auto-updater re-points ~/.jcode/builds/current back at the version it
#      was launched from. Replacing files without killing every process first
#      silently reverts the install a few minutes later. That happened on
#      prod-home-vm3 on 2026-07-31.
#   2. Old installs hide in more than one channel: current/, stable/,
#      shared-server/, and every directory under versions/. A client can pick
#      up a years-old binary through shared-server/ even when current/ is
#      correct.
#   3. Credentials and binaries both live under ~/.jcode. Deleting the wrong
#      subtree destroys OAuth accounts and API keys. This script deletes only
#      ~/.jcode/builds and refuses to run if it cannot back the secrets up
#      first.
#
# Usage: vm_purge_install.sh <tarball-or-binary> [--yes]

set -euo pipefail

artifact="${1:-}"
assume_yes="${2:-}"

if [[ -z "$artifact" ]]; then
    echo "usage: $0 <jcode-linux-x86_64.tar.gz | jcode binary> [--yes]" >&2
    exit 2
fi
if [[ ! -f "$artifact" ]]; then
    echo "error: artifact not found: $artifact" >&2
    exit 2
fi

jcode_dir="$HOME/.jcode"
builds_dir="$jcode_dir/builds"
config_dir="$HOME/.config/jcode"
launcher="$HOME/.local/bin/jcode"
stamp="$(date +%Y%m%d-%H%M%S)"
backup_dir="$HOME/jcode-cred-backup-$stamp"
workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

say() { printf '\n=== %s ===\n' "$*"; }

# ---------------------------------------------------------------- 1. inventory
say "1. what is installed and running right now"
echo "-- launcher --"
ls -l "$launcher" 2>/dev/null || echo "  (no $launcher)"
command -v jcode >/dev/null 2>&1 && echo "  on PATH: $(command -v jcode)" || echo "  (jcode not on PATH)"
echo "-- build channels --"
for ch in current stable shared-server; do
    if [[ -e "$builds_dir/$ch/jcode" ]]; then
        printf '  %-14s -> %s\n' "$ch" "$(readlink -f "$builds_dir/$ch/jcode" 2>/dev/null || echo '?')"
    else
        printf '  %-14s (absent)\n' "$ch"
    fi
done
echo "-- installed versions --"
ls -1 "$builds_dir/versions" 2>/dev/null | sed 's/^/  /' || echo "  (none)"
echo "-- running processes --"
pgrep -af 'jcode' 2>/dev/null | sed 's/^/  /' || echo "  (none)"

# --------------------------------------------------------------- 2. safety net
# Back up credentials BEFORE anything is deleted. If this fails we stop,
# because an unrecoverable auth.json is worse than a stale binary.
say "2. backing up credentials to $backup_dir"
mkdir -p "$backup_dir"
copied=0
for f in "$jcode_dir/auth.json" "$jcode_dir/config.toml" \
         "$jcode_dir/auth-refresh-state.json" "$jcode_dir/auth-validation.json"; do
    [[ -f "$f" ]] && { cp -a "$f" "$backup_dir/"; copied=$((copied + 1)); }
done
if [[ -d "$config_dir" ]]; then
    cp -a "$config_dir" "$backup_dir/config-jcode"
    copied=$((copied + 1))
fi
if [[ "$copied" -eq 0 ]]; then
    echo "  WARNING: no credential files found. Continuing (fresh host?)."
else
    ls -la "$backup_dir" | sed 's/^/  /'
fi

# --------------------------------------------------- 3. stage and prove the new build
# The new binary is unpacked and executed BEFORE anything is deleted. If the
# artifact is corrupt, truncated, built for the wrong architecture, or needs a
# glibc newer than this host, we find out while the working install is still
# in place, and exit without touching it. Purging first and discovering the
# problem afterwards would leave the VM with no jcode at all.
say "3. staging and smoke-testing the new build (nothing deleted yet)"
case "$artifact" in
    *.tar.gz | *.tgz)
        tar xzf "$artifact" -C "$workdir"
        # The tarball ships a small sh wrapper next to the real binary. Prefer
        # the explicit .bin, then the largest regular file. Selecting by size
        # threshold instead would silently reject a future smaller build.
        new_bin="$(find "$workdir" -type f -name '*.bin' | head -1)"
        if [[ -z "$new_bin" ]]; then
            new_bin="$(find "$workdir" -type f -exec ls -S {} + 2>/dev/null | head -1)"
        fi
        ;;
    *)
        new_bin="$artifact"
        ;;
esac
if [[ -z "${new_bin:-}" || ! -f "$new_bin" ]]; then
    echo "error: could not find a jcode binary inside $artifact" >&2
    exit 1
fi
chmod +x "$new_bin"

version_line="$("$new_bin" --version 2>/dev/null | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+[^ ]* \([0-9a-f]+\)' | head -1 || true)"
if [[ -z "$version_line" ]]; then
    echo "error: the new binary does not run on this host; nothing was changed" >&2
    echo "  attempted: $new_bin" >&2
    "$new_bin" --version 2>&1 | head -5 | sed 's/^/  /' >&2 || true
    exit 1
fi
semver="$(echo "$version_line" | sed 's/^v//; s/ .*//')"
echo "  new binary runs here and reports: $version_line"
echo "  will install as version: $semver"

# --------------------------------------------------------- 4. confirm the purge
# Ask before anything destructive happens, and in particular before any process
# is killed. An earlier version asked here but *after* the kill step, so a
# declined prompt still left the host with its servers stopped: an "abort" that
# aborted nothing. A refusal must now be a complete no-op.
#
# The prompt reads from the terminal if there is one, else stdin. It must never
# hard-require /dev/tty: over `ssh host 'bash script'` there is no controlling
# terminal, so opening /dev/tty fails and the run dies at the confirmation.
say "4. confirming"
confirm_purge() {
    local reply

    if [[ "$assume_yes" == "--yes" ]]; then
        echo "  --yes given, proceeding without confirmation"
        return 0
    fi

    # Probe /dev/tty in a subshell so a failed open cannot print to stderr and
    # cannot leave this shell's own stderr redirected. `[[ -r /dev/tty ]]` is
    # not a sufficient test: the device node exists under ssh even when the
    # session has no controlling terminal to open.
    if ( exec 3</dev/tty ) 2>/dev/null; then
        exec 3</dev/tty
        printf '  stop all jcode processes and delete %s? [y/N] ' "$builds_dir"
        read -r reply <&3 || reply=""
        exec 3<&-
    elif read -r -p "  stop all jcode processes and delete $builds_dir? [y/N] " reply; then
        : # answered over a pipe, e.g. `echo y | bash script`
    else
        # No terminal and no answer on stdin. This is the plain
        # `ssh host 'bash script'` case. Refuse rather than guess, and name the
        # flag that makes the intent explicit.
        echo
        echo "error: no terminal available to confirm a destructive purge." >&2
        echo "       nothing was changed. Re-run with --yes to proceed:" >&2
        echo "         bash $0 $artifact --yes" >&2
        return 1
    fi

    if [[ ! "$reply" =~ ^[Yy]$ ]]; then
        echo "  aborted; nothing was changed"
        return 1
    fi
    return 0
}
confirm_purge || exit 1

# ------------------------------------------------------------ 5. stop the fleet
# Every jcode process must die, including detached shared servers. A survivor
# both serves stale code to new clients and re-points the symlinks.
say "5. stopping every jcode process"

# Identify jcode processes by the executable they are actually running, not by
# a substring of their command line. A command-line match also catches this
# script (its own path contains "jcode"), any editor or tail watching a jcode
# file, and the ssh command that invoked it. Killing those instead of the
# server is both useless and destructive, so resolve /proc/<pid>/exe and keep
# only processes whose real binary is a jcode install.
jcode_pids() {
    local pid exe self_ancestors
    # Never kill this script or anything it descends from (the shell, sshd).
    self_ancestors=" $$ $PPID "
    pid=$PPID
    while [[ -n "$pid" && "$pid" != 0 && "$pid" != 1 ]]; do
        pid="$(awk '{print $4}' "/proc/$pid/stat" 2>/dev/null || echo)"
        [[ -n "$pid" ]] && self_ancestors+="$pid "
    done

    for pid in $(ls -1 /proc 2>/dev/null | grep -E '^[0-9]+$'); do
        [[ " $self_ancestors " == *" $pid "* ]] && continue
        exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
        [[ -n "$exe" ]] || continue
        case "$exe" in
            # Real jcode installs, including the pre-2026-07 .bin naming and
            # any binary left behind under ~/.jcode/builds.
            "$HOME"/.jcode/builds/*|"$HOME"/.local/bin/jcode|/usr/local/bin/jcode)
                echo "$pid" ;;
            */jcode|*/jcode-linux-x86_64|*/jcode-linux-x86_64.bin)
                echo "$pid" ;;
        esac
    done
}

kill_round() {
    local sig="$1" pids
    pids="$(jcode_pids | tr '\n' ' ')"
    [[ -n "${pids// /}" ]] || return 1
    echo "  $sig -> $pids"
    # shellcheck disable=SC2086
    kill "-$sig" $pids 2>/dev/null || true
    return 0
}

kill_round TERM && sleep 3 || echo "  (nothing running)"
kill_round KILL && sleep 1 || true
survivors="$(jcode_pids | tr '\n' ' ')"
if [[ -n "${survivors// /}" ]]; then
    echo "  survivors (inspect manually): $survivors"
    for p in $survivors; do
        printf '    %s -> %s\n' "$p" "$(readlink -f "/proc/$p/exe" 2>/dev/null)"
    done
else
    echo "  all jcode processes stopped"
fi

# ------------------------------------------------------------- 6. purge builds
# Only ~/.jcode/builds is removed. Sessions, logs, auth and config stay put.
say "6. removing all previously installed jcode binaries"
if [[ -d "$builds_dir" ]]; then
    du -sh "$builds_dir" 2>/dev/null | sed 's/^/  freeing: /'
    rm -rf "$builds_dir"
    echo "  removed $builds_dir"
else
    echo "  (no $builds_dir)"
fi
# Stray launchers and legacy copies that would shadow the new install.
for stray in "$launcher" "$HOME/bin/jcode" /usr/local/bin/jcode; do
    if [[ -e "$stray" || -L "$stray" ]]; then
        rm -f "$stray" 2>/dev/null && echo "  removed stray launcher $stray" \
            || echo "  could not remove $stray (needs sudo?)"
    fi
done

# ------------------------------------------------------------ 7. install fresh
say "7. installing the new build"
install_dir="$builds_dir/versions/$semver"
mkdir -p "$install_dir" "$builds_dir/current" "$builds_dir/stable" "$builds_dir/shared-server"
cp "$new_bin" "$install_dir/jcode"
chmod +x "$install_dir/jcode"

# All three channels point at the single installed version, so no channel can
# serve an older binary.
for ch in current stable shared-server; do
    ln -sfn "$install_dir/jcode" "$builds_dir/$ch/jcode"
    printf '%s\n' "$semver" > "$builds_dir/$ch-version"
done
echo "  installed to $install_dir/jcode"

# ------------------------------------------------- 8. launcher that cannot self-update
# These are release builds (JCODE_RELEASE_BUILD=1), so the background update
# check is active and will happily replace this binary with whatever GitHub
# publishes. The launcher pins --no-update so the version stays exactly what
# was deployed here.
say "8. installing an update-proof launcher"
mkdir -p "$(dirname "$launcher")"
cat > "$launcher" <<'LAUNCHER'
#!/usr/bin/env sh
# jcode launcher pinned to the locally deployed build.
#
# --no-update is forced because these binaries are release builds: without it
# the background updater downloads an upstream release and re-points
# ~/.jcode/builds/current, silently replacing the build that was deployed here.
# Remove this wrapper if you ever want auto-update back.
set -eu
for arg in "$@"; do
    if [ "$arg" = "--no-update" ]; then
        exec "$HOME/.jcode/builds/current/jcode" "$@"
    fi
done
exec "$HOME/.jcode/builds/current/jcode" --no-update "$@"
LAUNCHER
chmod +x "$launcher"
echo "  wrote $launcher"

case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "  NOTE: $HOME/.local/bin is not on PATH; add it to your shell rc" ;;
esac

# ------------------------------------------------------------------ 9. verify
say "9. verification"
hash -r 2>/dev/null || true
echo "-- resolved launcher --"
command -v jcode || echo "  jcode not on PATH"
echo "-- version --"
"$launcher" --version 2>&1 | grep -iE 'jcode v' | sed 's/^/  /' || true
echo "-- channels --"
for ch in current stable shared-server; do
    printf '  %-14s -> %s\n' "$ch" "$(readlink -f "$builds_dir/$ch/jcode" 2>/dev/null || echo MISSING)"
done
echo "-- installed versions (should be exactly one) --"
ls -1 "$builds_dir/versions" | sed 's/^/  /'
echo "-- credentials intact --"
for f in "$jcode_dir/auth.json" "$jcode_dir/config.toml"; do
    [[ -f "$f" ]] && printf '  ok  %s\n' "$f" || printf '  --  %s (absent)\n' "$f"
done
if [[ -f "$jcode_dir/auth.json" ]] && command -v python3 >/dev/null 2>&1; then
    python3 - "$jcode_dir/auth.json" <<'PY' 2>/dev/null || true
import json, sys
d = json.load(open(sys.argv[1]))
accounts = d.get("anthropic_accounts", [])
print(f"  providers: {list(d.keys())}")
print(f"  anthropic accounts: {len(accounts)}  active: {d.get('active_anthropic_account')}")
PY
fi
echo "-- running processes (should be none) --"
pgrep -af 'jcode' 2>/dev/null | sed 's/^/  /' || echo "  none"

say "done"
echo "Deployed: $version_line"
echo "Binary:   $install_dir/jcode"
echo "Backup:   $backup_dir"
