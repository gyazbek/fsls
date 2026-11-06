#!/usr/bin/env bash
# Real hung-mount check for fsls: mounts a FUSE file system whose daemon never answers
# (every statfs on it blocks, like a dead hard-mounted NFS server) and checks that fsls
# still finishes, marking it "timed out". Needs root and python3.
# Usage: sudo scripts/test-hung-mount.sh path/to/fsls
set -euo pipefail

FSLS=${1:?usage: $0 path/to/fsls}
MNT=/mnt/fsls-hung-test
PID=

cleanup() {
    set +e
    [ -n "$PID" ] && kill "$PID" 2>/dev/null && wait "$PID" 2>/dev/null
    umount -l "$MNT" 2>/dev/null
    rmdir "$MNT" 2>/dev/null
}
trap cleanup EXIT

mkdir -p "$MNT"
# Open /dev/fuse, mount it, then never read a request: the kernel waits forever.
python3 - "$MNT" <<'EOF' &
import ctypes, os, sys, time
mnt = sys.argv[1].encode()
fd = os.open("/dev/fuse", os.O_RDWR)
opts = f"fd={fd},rootmode=40000,user_id=0,group_id=0".encode()
libc = ctypes.CDLL(None, use_errno=True)
if libc.mount(b"fsls-hung", mnt, b"fuse", 0, opts) != 0:
    sys.exit("mount failed: " + os.strerror(ctypes.get_errno()))
time.sleep(600)
EOF
PID=$!
for _ in $(seq 50); do grep -q " $MNT " /proc/mounts && break; sleep 0.1; done
grep -q " $MNT " /proc/mounts || { echo "FUSE mount did not appear"; exit 1; }

fail() { echo "FAIL: $*"; exit 1; }
# Runs fsls, prints its output, fails if it took longer than $1 seconds.
timed() {
    local max=$1; shift
    local start end rc=0
    start=$(date +%s.%N)
    OUT=$("$FSLS" "$@" 2>&1) || rc=$?
    end=$(date +%s.%N)
    echo "$OUT"
    awk -v s="$start" -v e="$end" -v m="$max" 'BEGIN { printf "took %.2fs\n", e - s; exit !(e - s < m) }' \
        || fail "took longer than ${max}s"
    return $rc
}

echo "=== fsls --timeout 1   (a plain statfs here would hang forever)"
timed 4 --timeout 1 -k unknown,fixed
grep -qE "^$MNT .*timed out" <<<"$OUT" || fail "hung mount not shown as timed out"
grep -q "warning: $MNT: no response" <<<"$OUT" || fail "no warning"
grep -qE '^/ ' <<<"$OUT" || fail "healthy root missing"

echo; echo "=== fsls --timeout 1 $MNT/some/file   (path lookup inside the hung mount blocks)"
rc=0; timed 5 --timeout 1 "$MNT/some/file" || rc=$?
[ "$rc" = 1 ] || fail "expected exit 1, got $rc"
grep -q "no response within" <<<"$OUT" || fail "no timeout message"

echo; echo "=== json"
timed 4 --timeout 0.5 --json -k unknown >/dev/null
grep -q '"timed_out": true' <<<"$OUT" || fail "json lacks timed_out"

echo; echo "PASS"
