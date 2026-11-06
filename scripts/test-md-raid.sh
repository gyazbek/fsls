#!/usr/bin/env bash
# Real Linux software-RAID check for fsls: builds an md RAID1 from two loop devices,
# shows it healthy, fails a member, shows the rebuild, then tears everything down.
# Needs root and mdadm.   Usage: sudo scripts/test-md-raid.sh path/to/fsls
set -euo pipefail

FSLS=${1:?usage: $0 path/to/fsls}
MD=/dev/md127
MNT=/mnt/fsls-raid-test
IMG=/tmp/fsls-raid-test
SPEED=$(cat /proc/sys/dev/raid/speed_limit_max)
L0=''
L1=''

cleanup() {
    set +e
    echo "$SPEED" >/proc/sys/dev/raid/speed_limit_max
    umount "$MNT" 2>/dev/null
    mdadm --stop "$MD" >/dev/null 2>&1
    [ -n "$L0" ] && losetup -d "$L0"
    [ -n "$L1" ] && losetup -d "$L1"
    rm -f "$IMG"-0.img "$IMG"-1.img
    rmdir "$MNT" 2>/dev/null
}
trap cleanup EXIT

step() { echo; echo "=== $*"; }
OUT=
# Never aborts on a non-zero fsls exit (set -e would end the script silently); expect() reports.
run() { OUT=$("$FSLS" "$@" 2>&1) || true; echo "$OUT"; }
expect() { grep -qE -- "$1" <<<"$OUT" || { echo "FAIL: expected /$1/"; exit 1; }; }

truncate -s 256M "$IMG"-0.img "$IMG"-1.img
L0=$(losetup -f --show "$IMG"-0.img)
L1=$(losetup -f --show "$IMG"-1.img)
N0=$(basename "$L0") N1=$(basename "$L1")
mdadm --create "$MD" --level=1 --raid-devices=2 --metadata=1.2 --run --quiet "$L0" "$L1"
mdadm --wait "$MD" || true
mkfs.ext4 -q "$MD"
mkdir -p "$MNT"
mount "$MD" "$MNT"

step "healthy RAID1"
run -d "$MNT"
expect "raid1: ok"
expect "> $N0 "
expect "> $N1 "

step "member $L1 failed"
mdadm "$MD" --fail "$L1" --quiet
run -d "$MNT"
expect "raid1: degraded 1/2 missing"
expect "> $N1 .*faulty"
expect "warning: .*degraded"

step "member removed and re-added: rebuilding (throttled so it is visible)"
mdadm "$MD" --remove "$L1" --quiet
echo 2000 >/proc/sys/dev/raid/speed_limit_max
mdadm "$MD" --add "$L1" --quiet
sleep 2
run -d "$MNT"
expect "recover [0-9]+%"
expect "> $N1 .*rebuilding"
run --json "$MNT"
expect '"raid_health": \{"ok": false'

step "rebuild finished"
echo "$SPEED" >/proc/sys/dev/raid/speed_limit_max
mdadm --wait "$MD" || true
run -d "$MNT"
expect "raid1: ok"

echo; echo "PASS"
