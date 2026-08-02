#!/usr/bin/env bash
#
# Tests for scripts/vm_purge_install.sh, run inside a throwaway Linux container.
#
#   docker run --rm \
#     -v "$PWD/scripts/test_vm_purge_install.sh:/t.sh:ro" \
#     -v "$PWD/scripts/vm_purge_install.sh:/tmp/vm_purge_install.sh:ro" \
#     ubuntu:22.04 bash /t.sh
#
# The suite simulates prod-home-vm3's real dirty layout: several stale versions
# across current/, stable/ and shared-server/, the pre-2026-07
# jcode-linux-x86_64.bin naming, live processes holding an old binary open, and
# credentials that must survive. It then asserts the failure paths leave a
# working install untouched, because a purge that aborts halfway would strand
# the host with no jcode at all.
set -uo pipefail

fail=0
check() { if [ "$1" = "$2" ]; then echo "  PASS: $3"; else echo "  FAIL: $3 (want '$2' got '$1')"; fail=1; fi; }

export HOME=/root
mkdir -p "$HOME/.jcode/builds/versions/0.64.2" \
         "$HOME/.jcode/builds/versions/0.60.263" \
         "$HOME/.jcode/builds/versions/deaaf67-rebased" \
         "$HOME/.jcode/builds/versions/a6d0bb0-fablefix" \
         "$HOME/.jcode/builds/current" "$HOME/.jcode/builds/stable" \
         "$HOME/.jcode/builds/shared-server" \
         "$HOME/.config/jcode" "$HOME/.local/bin" "$HOME/.jcode/sessions"

# Real ELF binaries that sleep, so /proc/<pid>/exe points at a jcode path.
for v in 0.64.2 0.60.263 deaaf67-rebased a6d0bb0-fablefix; do
  cp /bin/sleep "$HOME/.jcode/builds/versions/$v/jcode"
done
cp /bin/sleep "$HOME/.jcode/builds/versions/0.64.2/jcode-linux-x86_64.bin"   # legacy naming
ln -sfn "$HOME/.jcode/builds/versions/0.64.2/jcode" "$HOME/.jcode/builds/current/jcode"
ln -sfn "$HOME/.jcode/builds/versions/0.64.2/jcode" "$HOME/.jcode/builds/stable/jcode"
ln -sfn "$HOME/.jcode/builds/versions/a6d0bb0-fablefix/jcode" "$HOME/.jcode/builds/shared-server/jcode"
ln -sfn "$HOME/.jcode/builds/current/jcode" "$HOME/.local/bin/jcode"
echo 0.64.2 > "$HOME/.jcode/builds/current-version"

cat > "$HOME/.jcode/auth.json" <<'J'
{"anthropic_accounts":[{"label":"claude-1"},{"label":"claude-2"},{"label":"claude-3"},{"label":"claude-4"}],"active_anthropic_account":"claude-1"}
J
echo 'model = "sonnet"' > "$HOME/.jcode/config.toml"
echo 'DASHSCOPE_API_KEY=secret-do-not-lose' > "$HOME/.config/jcode/dashscope.env"
echo '{"session":"keepme"}' > "$HOME/.jcode/sessions/session_keep.json"
auth_before=$(md5sum "$HOME/.jcode/auth.json" | cut -d' ' -f1)
key_before=$(md5sum "$HOME/.config/jcode/dashscope.env" | cut -d' ' -f1)

# The stale shared server (like PID 2771): genuinely running a jcode binary.
"$HOME/.jcode/builds/versions/0.64.2/jcode-linux-x86_64.bin" 6000 & SRV=$!
# A second one via the legacy current/ symlink.
"$HOME/.jcode/builds/current/jcode" 6000 & SRV2=$!
# DECOY: command line mentions jcode but the exe is /bin/sleep. Must SURVIVE:
# killing this class of process is what a naive pgrep -f would do.
cp /bin/sleep /tmp/watcher-not-jcode
/tmp/watcher-not-jcode 6000 & DECOY=$!
sleep 1
echo "  server pids: $SRV $SRV2 ; decoy: $DECOY"

# The "new build": a real ELF so --version works and it is not size-filtered out.
mkdir -p /tmp/pkg
cat > /tmp/mkbin.c <<'C'
#include <stdio.h>
int main(int c, char**v){ printf("jcode v0.60.264 (e38b61881)\n"); return 0; }
C
if command -v cc >/dev/null 2>&1; then cc -o /tmp/pkg/jcode-linux-x86_64.bin /tmp/mkbin.c
else printf '#!/bin/sh\necho "jcode v0.60.264 (e38b61881)"\n' > /tmp/pkg/jcode-linux-x86_64.bin; fi
chmod +x /tmp/pkg/jcode-linux-x86_64.bin
printf '#!/bin/sh\nexec "$(dirname "$0")/jcode-linux-x86_64.bin" "$@"\n' > /tmp/pkg/jcode-linux-x86_64
chmod +x /tmp/pkg/jcode-linux-x86_64
tar czf /tmp/jcode-linux-x86_64.tar.gz -C /tmp/pkg jcode-linux-x86_64 jcode-linux-x86_64.bin

echo "=== RUNNING PURGE SCRIPT ==="
bash /tmp/vm_purge_install.sh /tmp/jcode-linux-x86_64.tar.gz --yes > /tmp/purge.log 2>&1
rc=$?
sed -n '/3. stopping/,$p' /tmp/purge.log | head -45
echo "=== ASSERTIONS (exit $rc) ==="
check "$rc" "0" "script exits 0"
check "$(ls -1 $HOME/.jcode/builds/versions | wc -l | tr -d ' ')" "1" "exactly one version remains"
check "$(ls -1 $HOME/.jcode/builds/versions)" "0.60.264" "remaining version is the new one"
for ch in current stable shared-server; do
  check "$(readlink -f $HOME/.jcode/builds/$ch/jcode)" "$HOME/.jcode/builds/versions/0.60.264/jcode" "$ch points at new build"
done
check "$(md5sum $HOME/.jcode/auth.json | cut -d' ' -f1)" "$auth_before" "auth.json untouched"
check "$(md5sum $HOME/.config/jcode/dashscope.env | cut -d' ' -f1)" "$key_before" "api key env untouched"
check "$([ -f $HOME/.jcode/sessions/session_keep.json ] && echo yes)" "yes" "sessions preserved"
check "$(kill -0 $SRV 2>/dev/null && echo alive || echo dead)" "dead" "stale shared server killed"
check "$(kill -0 $SRV2 2>/dev/null && echo alive || echo dead)" "dead" "stale current-channel proc killed"
check "$(kill -0 $DECOY 2>/dev/null && echo alive || echo dead)" "alive" "non-jcode decoy NOT killed"
check "$(grep -c 'no-update' $HOME/.local/bin/jcode)" "3" "launcher forces --no-update"
check "$($HOME/.local/bin/jcode --version 2>&1 | grep -c '0.60.264')" "1" "launcher runs the new build"
check "$(ls -d $HOME/jcode-cred-backup-* >/dev/null 2>&1 && echo yes)" "yes" "credential backup created"
check "$([ -e $HOME/.jcode/builds/versions/0.64.2 ] && echo present || echo gone)" "gone" "old 0.64.2 gone"
# The whole point: nothing can resurrect the old version.
check "$(grep -rl '0.64.2' $HOME/.jcode/builds 2>/dev/null | wc -l | tr -d ' ')" "0" "no reference to old version left in builds/"


echo
echo "########## FAILURE-PATH SUITE ##########"
rm -rf "$HOME/.jcode/builds" "$HOME/.local/bin/jcode"
export HOME=/root
mkdir -p "$HOME/.jcode/builds/versions/0.64.2" "$HOME/.jcode/builds/current" "$HOME/.local/bin" "$HOME/.config/jcode"
cp /bin/sleep "$HOME/.jcode/builds/versions/0.64.2/jcode"
ln -sfn "$HOME/.jcode/builds/versions/0.64.2/jcode" "$HOME/.jcode/builds/current/jcode"
ln -sfn "$HOME/.jcode/builds/current/jcode" "$HOME/.local/bin/jcode"
echo '{"anthropic_accounts":[{"label":"claude-1"}]}' > "$HOME/.jcode/auth.json"

echo "=== CASE A: corrupt tarball ==="
head -c 500 /dev/urandom > /tmp/corrupt.tar.gz
bash /tmp/vm_purge_install.sh /tmp/corrupt.tar.gz --yes >/tmp/a.log 2>&1; rcA=$?
check "$([ "$rcA" != "0" ] && echo nonzero)" "nonzero" "A: exits nonzero on corrupt artifact"
check "$([ -e $HOME/.jcode/builds/versions/0.64.2/jcode ] && echo present)" "present" "A: existing install NOT deleted"
check "$(readlink -f $HOME/.local/bin/jcode)" "$HOME/.jcode/builds/versions/0.64.2/jcode" "A: launcher still works"

echo "=== CASE B: binary that cannot execute (wrong arch / bad glibc) ==="
mkdir -p /tmp/badpkg && head -c 200000 /dev/urandom > /tmp/badpkg/jcode-linux-x86_64.bin
chmod +x /tmp/badpkg/jcode-linux-x86_64.bin
tar czf /tmp/bad.tar.gz -C /tmp/badpkg jcode-linux-x86_64.bin
bash /tmp/vm_purge_install.sh /tmp/bad.tar.gz --yes >/tmp/b.log 2>&1; rcB=$?
grep -i 'does not run on this host' /tmp/b.log >/dev/null && msgB=yes || msgB=no
check "$([ "$rcB" != "0" ] && echo nonzero)" "nonzero" "B: exits nonzero on unrunnable binary"
check "$msgB" "yes" "B: explains the binary cannot run here"
check "$([ -e $HOME/.jcode/builds/versions/0.64.2/jcode ] && echo present)" "present" "B: existing install NOT deleted"
check "$(readlink -f $HOME/.local/bin/jcode)" "$HOME/.jcode/builds/versions/0.64.2/jcode" "B: launcher still works"
check "$([ -f $HOME/.jcode/auth.json ] && echo yes)" "yes" "B: credentials intact"

echo "=== CASE C: missing artifact ==="
bash /tmp/vm_purge_install.sh /tmp/nope.tar.gz --yes >/tmp/c.log 2>&1; rcC=$?
check "$([ "$rcC" != "0" ] && echo nonzero)" "nonzero" "C: exits nonzero"
check "$([ -e $HOME/.jcode/builds/versions/0.64.2/jcode ] && echo present)" "present" "C: existing install NOT deleted"

echo "=== CASE D: idempotent - run twice, second run is a no-op upgrade ==="
mkdir -p /tmp/pkg
cat > /tmp/mk.c <<'C'
#include <stdio.h>
int main(){ printf("jcode v0.60.264 (e38b61881)\n"); return 0; }
C
cc -o /tmp/pkg/jcode-linux-x86_64.bin /tmp/mk.c 2>/dev/null || printf '#!/bin/sh\necho "jcode v0.60.264 (e38b61881)"\n' > /tmp/pkg/jcode-linux-x86_64.bin
chmod +x /tmp/pkg/jcode-linux-x86_64.bin
tar czf /tmp/good.tar.gz -C /tmp/pkg jcode-linux-x86_64.bin
bash /tmp/vm_purge_install.sh /tmp/good.tar.gz --yes >/tmp/d1.log 2>&1; rcD1=$?
bash /tmp/vm_purge_install.sh /tmp/good.tar.gz --yes >/tmp/d2.log 2>&1; rcD2=$?
check "$rcD1" "0" "D: first install ok"
check "$rcD2" "0" "D: second (repeat) install ok"
check "$(ls -1 $HOME/.jcode/builds/versions | wc -l | tr -d ' ')" "1" "D: still exactly one version after two runs"
check "$($HOME/.local/bin/jcode --version 2>&1 | grep -c '0.60.264')" "1" "D: launcher works after repeat run"
# the launcher must not double-add --no-update when the user passes it
check "$($HOME/.local/bin/jcode --no-update --version 2>&1 | grep -c '0.60.264')" "1" "D: explicit --no-update still works"

echo "=== RESULT: $([ $fail -eq 0 ] && echo ALL PASS || echo FAILURES) ==="
exit $fail
