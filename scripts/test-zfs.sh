#!/usr/bin/env bash
# Real ZFS check for fsls: builds a mirrored pool from two files with three datasets,
# writes data to two of them, and asserts that `--total` adds the datasets' used space
# but counts the pool's free space once (every dataset reports the whole pool as free).
# Needs root, zfsutils-linux and the zfs kernel module.
# Usage: sudo scripts/test-zfs.sh path/to/fsls
set -euo pipefail

FSLS=${1:?usage: $0 path/to/fsls}
POOL=fslszfstest
MNT=/mnt/fsls-zfs-test
IMG=/tmp/fsls-zfs-test

if ! modprobe zfs 2>/dev/null || ! command -v zpool >/dev/null; then
    echo "SKIP: no zfs kernel module or zpool command"
    exit 0
fi

cleanup() {
    set +e
    zpool destroy -f "$POOL" 2>/dev/null
    rm -f "$IMG"-0.img "$IMG"-1.img
    rmdir "$MNT" 2>/dev/null
}
trap cleanup EXIT

step() { echo; echo "=== $*"; }
fail() { echo "FAIL: $*"; exit 1; }
OUT=
run() { OUT=$("$FSLS" "$@" 2>&1) || true; echo "$OUT"; }
expect() { grep -qE -- "$1" <<<"$OUT" || fail "expected /$1/"; }
# |a - b| <= tolerance
near() { local d=$(($1 - $2)); [ "${d#-}" -le "$3" ]; }

truncate -s 512M "$IMG"-0.img "$IMG"-1.img
zpool create -f -m "$MNT" "$POOL" mirror "$IMG"-0.img "$IMG"-1.img
zfs create "$POOL/a"
zfs create "$POOL/b"
dd if=/dev/urandom of="$MNT/a/data" bs=1M count=64 status=none
dd if=/dev/urandom of="$MNT/b/data" bs=1M count=32 status=none
sync
zpool sync "$POOL"

step "datasets listed"
run -t zfs
expect "^$MNT "
expect "^$MNT/a "
expect "^$MNT/b "

step "total counts the pool's free space once"
run -t zfs --total -b -o mount,size,used,avail
read -r SIZE USED AVAIL < <(awk '$1 == "total" { print $2, $3, $4 }' <<<"$OUT") ||
    fail "no total row"
POOL_AVAIL=$(zfs get -Hp -o value available "$POOL")
REFERENCED=$(zfs list -Hp -r -o referenced "$POOL" | awk '{ s += $1 } END { print s }')
echo "fsls: size $SIZE used $USED avail $AVAIL"
echo "zfs:  referenced $REFERENCED available $POOL_AVAIL"
MIB=$((1024 * 1024))
near "$AVAIL" "$POOL_AVAIL" "$MIB" || fail "avail $AVAIL is not the pool's $POOL_AVAIL"
near "$USED" "$REFERENCED" "$MIB" || fail "used $USED is not the datasets' $REFERENCED"
[ "$SIZE" -eq $((USED + AVAIL)) ] || fail "size $SIZE != used + avail"
[ "$USED" -ge $((96 * MIB)) ] || fail "used $USED is less than the 96 MiB written"

echo; echo "PASS"
