#!/usr/bin/env bash
# Real AppleRAID check for fsls (macOS): builds a mirror from two RAM disks and takes it
# through healthy -> member removed -> member re-added (rebuild) -> member disk detached.
# At every step fsls must agree with `diskutil appleRAID list` about the set's status and
# members (AppleRAID decides whether the set stays mounted). Deletes everything at the end.
# Needs root.   Usage: sudo scripts/test-apple-raid.sh path/to/fsls
set -euo pipefail

FSLS=${1:?usage: $0 path/to/fsls}
NAME=fslsraid
MNT="/Volumes/$NAME"
D0=''
D1=''
# The set's Unique ID. Commands must use it, not the name: a removed member keeps a copy of
# the volume with the same name, and diskutil then resolves the name to that copy.
SET=''

cleanup() {
    set +e
    [ -n "$SET" ] && diskutil appleRAID delete "$SET" >/dev/null 2>&1
    for d in $D0 $D1; do hdiutil detach "$d" -force >/dev/null 2>&1; done
}
trap cleanup EXIT

step() { echo; echo "=== $*"; }
fail() { echo "FAIL: $*"; exit 1; }
OUT=''
RC=0
# Never aborts: fsls exiting non-zero is something the checks look at, not a script error.
run() { RC=0; OUT=$("$FSLS" "$@" 2>&1) || RC=$?; echo "$OUT"; }
expect() { grep -qE -- "$1" <<<"$OUT" || fail "expected /$1/"; }

# This set's block of `diskutil appleRAID list` (by Unique ID once known, by name before).
set_block() {
    diskutil appleRAID list 2>/dev/null |
        awk -v n="$NAME" -v id="$SET" '/^Name:/ { s = (id == "" && $2 == n) } /^Unique ID:/ { if (id != "") s = ($3 == id) } s'
}
set_status() { set_block | awk '/^Status:/ { sub(/^Status: */, ""); print; exit }'; }
# Whole disks of the members diskutil lists with a device node (disk6s1 -> disk6).
set_members() { set_block | awk '$1 ~ /^([0-9]+|-)$/ && $2 ~ /^disk/ { sub(/s[0-9]+$/, "", $2); print $2 }'; }

# fsls must report what diskutil reports.
check() {
    local status lc
    status=$(set_status)
    set_block
    echo "-- diskutil status: ${status:-<no set>}"
    # Exact mount point: the removed member's copy mounts as "/Volumes/fslsraid 1".
    if mount | grep -qF " on $MNT ("; then
        run -d "$MNT"
        [ "$RC" = 0 ] || fail "fsls exited $RC"
        case "$status" in
            "") fail "volume is mounted but diskutil lists no set" ;;
            Online) expect "mirror: ok" ;;
            *)
                lc=$(tr '[:upper:]' '[:lower:]' <<<"$status")
                expect "mirror: $lc"
                expect "warning: .*mirror array is $lc"
                ;;
        esac
        for m in $(set_members); do expect "> $m "; done
    else
        echo "-- $MNT is not mounted"
        run -a -d
        [ "$RC" = 0 ] || fail "fsls -a -d exited $RC"
        if grep -q "^$MNT " <<<"$OUT"; then fail "unmounted volume still listed"; fi
        run "$MNT"
        [ "$RC" = 1 ] || fail "expected exit 1 for a path on an unmounted volume, got $RC"
    fi
}

# 512 MB RAM disks (ram:// counts 512-byte sectors).
D0=$(hdiutil attach -nomount ram://1048576 | awk '{print $1; exit}')
D1=$(hdiutil attach -nomount ram://1048576 | awk '{print $1; exit}')
N0=$(basename "$D0") N1=$(basename "$D1")
diskutil appleRAID create mirror "$NAME" JHFS+ "$N0" "$N1"
for _ in $(seq 30); do [ -d "$MNT" ] && break; sleep 1; done
SET=$(set_block | awk '/^Unique ID:/ { print $3; exit }')
[ -n "$SET" ] || fail "could not find the new set's Unique ID"

step "healthy mirror"
check
[ "$(set_status)" = Online ] || fail "new set is not Online"
expect "> $N0 "
expect "> $N1 "

step "member ${N1}s1 removed with diskutil appleRAID remove"
if diskutil appleRAID remove "${N1}s1" "$SET"; then
    sleep 3
    check
    if grep -q "> $N1 " <<<"$OUT"; then fail "removed member $N1 still listed"; fi

    step "$N1 added back (rebuild)"
    if diskutil appleRAID add member "$N1" "$SET"; then
        sleep 2
        check
        for _ in $(seq 60); do [ "$(set_status)" = Online ] && break; sleep 2; done
        step "after rebuild"
        check
    else
        echo "(add member failed; continuing)"
    fi
else
    echo "(remove failed; continuing)"
fi

step "a member's disk detached"
victim=$(set_members | tail -1)
[ -n "$victim" ] || fail "no member left to detach"
[ "$(set_members | wc -l)" -gt 1 ] || echo "(only one member left: detaching it destroys the set)"
hdiutil detach "/dev/$victim" -force
[ "/dev/$victim" = "$D1" ] && D1=
[ "/dev/$victim" = "$D0" ] && D0=
sleep 3
check

echo; echo "PASS"
