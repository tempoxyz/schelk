#!/bin/sh
# schelk QEMU integration test runner.
#
# This script runs as PID 1 (/init) inside a minimal QEMU VM. It exercises
# schelk through every user story described in SPEC.md and README.md.
#
# Results go to serial console (ttyS0). The host script (ci/run-vm-tests.sh)
# looks for SCHELK_TEST_RESULT=PASS/FAIL to determine the outcome.

set -u

export PATH=/bin:/sbin:/usr/sbin:/usr/bin
export HOME=/tmp

# ── Bootstrap ─────────────────────────────────────────────────────────

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
mkdir -p /dev/mapper

export DM_DISABLE_UDEV=1
dmsetup mknodes 2>/dev/null || true
mv /sbin/dmsetup /sbin/dmsetup.real
cat > /sbin/dmsetup << 'WRAPPER'
#!/bin/sh
/sbin/dmsetup.real --noudevsync "$@"
rc=$?
case "$1" in
    create|remove)
        [ "$rc" -eq 0 ] && /sbin/dmsetup.real --noudevsync mknodes >/dev/null 2>&1
        ;;
esac
exit $rc
WRAPPER
chmod +x /sbin/dmsetup

# ── Module loader ─────────────────────────────────────────────────────

load_mod() {
    local name="$1"
    shift
    if [ -d "/sys/module/${name}" ] || [ -d "/sys/module/$(echo "$name" | tr '-' '_')" ]; then
        return 0
    fi
    local mod_file
    mod_file=$(find /lib/modules -name "${name}.ko*" 2>/dev/null | head -1)
    [ -z "$mod_file" ] && return 0
    local decompressed="/tmp/${name}.ko"
    case "$mod_file" in
        *.zst) zstd -d -q "$mod_file" -o "$decompressed" 2>/dev/null ;;
        *.xz)  xz -d -q -k "$mod_file" -c > "$decompressed" 2>/dev/null ;;
        *.gz)  gzip -d -q -k "$mod_file" -c > "$decompressed" 2>/dev/null ;;
        *.ko)  decompressed="$mod_file" ;;
    esac
    insmod "$decompressed" "$@" 2>/dev/null
}

load_mod crc32c_generic
load_mod libcrc32c
load_mod crc16
load_mod mbcache
load_mod jbd2
load_mod ext4
load_mod loop
load_mod dm-mod
load_mod dm-bufio
load_mod dm-persistent-data
load_mod dm-era
load_mod brd rd_nr=1 rd_size=8192

# ── Test harness ──────────────────────────────────────────────────────

PASSED=0
FAILED=0
ERRORS=""

pass() {
    PASSED=$((PASSED + 1))
    echo "  PASS: $1"
}

fail() {
    FAILED=$((FAILED + 1))
    ERRORS="$ERRORS\n  FAIL: $1: $2"
    echo "  FAIL: $1: $2"
}

story() {
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "$1"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""
}

# Run a command, capture output, return its real exit code.
# Usage: run_schelk schelk mount ...
# Then check $LAST_RC and $LAST_OUT.
LAST_RC=0
LAST_OUT=""
run_schelk() {
    LAST_OUT=$("$@" 2>&1)
    LAST_RC=$?
    printf '%s\n' "$LAST_OUT"
    return $LAST_RC
}

# Assert command succeeds (exit 0).
assert_ok() {
    local name="$1"
    shift
    run_schelk "$@"
    [ $LAST_RC -eq 0 ] && pass "$name" || fail "$name" "exit $LAST_RC"
}

# Assert command fails (exit != 0).
assert_fail() {
    local name="$1"
    shift
    run_schelk "$@"
    [ $LAST_RC -ne 0 ] && pass "$name" || fail "$name" "expected failure but got exit 0"
}

# Check if a path is a real mountpoint.
is_mounted() {
    grep -q "[[:space:]]$1[[:space:]]" /proc/mounts
}

# Create loop-backed volumes in $1 and set VIRGIN, SCRATCH, RAMDISK.
setup_volumes() {
    local dir="$1"
    rm -rf "$dir"
    mkdir -p "$dir"
    dd if=/dev/zero of="$dir/virgin.img" bs=1M count=32 2>/dev/null
    dd if=/dev/zero of="$dir/scratch.img" bs=1M count=32 2>/dev/null
    dd if=/dev/zero of="$dir/ramdisk.img" bs=1M count=4 2>/dev/null
    VIRGIN=$(/sbin/losetup --find --show "$dir/virgin.img")
    SCRATCH=$(/sbin/losetup --find --show "$dir/scratch.img")
    RAMDISK=$(/sbin/losetup --find --show "$dir/ramdisk.img")
}

# Full teardown: unmount, remove dm devices, detach loops, free tmpfs.
teardown() {
    umount /tmp/mnt 2>/dev/null || true
    umount /tmp/mnt_a 2>/dev/null || true
    umount /tmp/mnt_b 2>/dev/null || true
    dmsetup remove bench_era 2>/dev/null || true
    dmsetup remove era_a 2>/dev/null || true
    dmsetup remove era_b 2>/dev/null || true
    /sbin/losetup -D 2>/dev/null || true
    rm -f /var/lib/schelk/state.json
    rm -rf /tmp/state_a /tmp/state_b
    rm -rf "$@" 2>/dev/null || true
}

MP="/tmp/mnt"
mkdir -p "$MP"

echo "============================================"
echo "schelk QEMU integration tests"
echo "============================================"

###########################################################################
# Story 0: Prerequisites
#
# Verify the VM environment is functional before running any schelk tests.
###########################################################################
story "STORY 0: Prerequisites"

schelk --help > /dev/null 2>&1 && pass "schelk binary" || fail "schelk binary" "failed to execute"
dmsetup version > /dev/null 2>&1 && pass "dmsetup" || fail "dmsetup" "not functional"
dmsetup targets 2>/dev/null | grep -q "era" && pass "dm-era target" || fail "dm-era target" "not available"

###########################################################################
# Story 1: Fresh start — init-new → mount → bench → recover
#
# The basic benchmarking workflow from README.md:
# Create fresh volumes, run a benchmark, then recover to the pristine state.
###########################################################################
story "STORY 1: Fresh start (init-new → mount → bench → recover)"

teardown /tmp/s1
setup_volumes /tmp/s1

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

assert_ok "mount" schelk mount

is_mounted "$MP" && pass "mountpoint is live" || fail "mountpoint" "not in /proc/mounts"

echo "benchmark data" > "$MP/bench.txt"
dd if=/dev/urandom of="$MP/workload.bin" bs=4k count=100 2>/dev/null
sync
pass "benchmark write (400KB)"

assert_ok "recover" schelk recover

assert_ok "remount" schelk mount
is_mounted "$MP" || fail "remount" "not mounted"

if [ ! -f "$MP/bench.txt" ] && [ ! -f "$MP/workload.bin" ]; then
    pass "recovery correctness (benchmark files removed)"
else
    fail "recovery correctness" "files survived recover"
fi
teardown /tmp/s1

###########################################################################
# Story 2: Adopt existing volume — init-from → mount → bench → recover
#
# From SPEC.md init-from: user has a pre-populated virgin volume (e.g. a
# database snapshot). schelk adopts it and copies to scratch.
###########################################################################
story "STORY 2: Adopt existing volume (init-from)"

teardown /tmp/s2
setup_volumes /tmp/s2

# Pre-populate virgin
mkfs.ext4 -F -b 4096 -L schelk "$VIRGIN" >/dev/null 2>&1
mkdir -p /tmp/s2_mnt
mount "$VIRGIN" /tmp/s2_mnt
echo "initial db state" > /tmp/s2_mnt/snapshot.txt
mkdir -p /tmp/s2_mnt/data
dd if=/dev/urandom of=/tmp/s2_mnt/data/block.bin bs=4k count=50 2>/dev/null
sync
umount /tmp/s2_mnt

assert_ok "init-from" schelk init-from \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" --fstype ext4 -y

assert_ok "mount" schelk mount
is_mounted "$MP" || fail "mount" "not in /proc/mounts"

[ -f "$MP/snapshot.txt" ] && [ -d "$MP/data" ] && \
    pass "pre-existing data preserved" || fail "pre-existing data" "missing"

echo "modified" >> "$MP/snapshot.txt"
rm -rf "$MP/data"
sync

assert_ok "recover" schelk recover
assert_ok "remount" schelk mount

content=$(cat "$MP/snapshot.txt" 2>/dev/null)
if [ "$content" = "initial db state" ] && [ -d "$MP/data" ]; then
    pass "original data restored after recover"
else
    fail "data restoration" "content='$content'"
fi
teardown /tmp/s2

###########################################################################
# Story 3: Adopt without copy — init-from --no-copy
#
# From SPEC.md: user has manually prepared identical volumes and wants to
# skip the expensive full copy. schelk validates and saves state only.
###########################################################################
story "STORY 3: Adopt without copy (init-from --no-copy)"

teardown /tmp/s3
setup_volumes /tmp/s3

mkfs.ext4 -F -b 4096 -L schelk "$VIRGIN" >/dev/null 2>&1
mkdir -p /tmp/s3_mnt
mount "$VIRGIN" /tmp/s3_mnt
echo "no-copy data" > /tmp/s3_mnt/data.txt
sync
umount /tmp/s3_mnt

# User manually copies virgin to scratch
dd if="$VIRGIN" of="$SCRATCH" bs=1M 2>/dev/null

assert_ok "init-from --no-copy" schelk init-from \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" --fstype ext4 --no-copy -y

assert_ok "mount" schelk mount
is_mounted "$MP" || fail "mount" "not mounted"
[ -f "$MP/data.txt" ] && \
    pass "data accessible after no-copy init" || fail "data access" "missing"

echo "new" > "$MP/new.txt"
sync
assert_ok "recover" schelk recover
assert_ok "remount" schelk mount
[ -f "$MP/data.txt" ] && [ ! -f "$MP/new.txt" ] && \
    pass "recover works after no-copy init" || fail "recover after no-copy" "bad state"
teardown /tmp/s3

###########################################################################
# Story 4: Multi-iteration bench loop — (mount → bench → recover) × N
#
# From README.md: the core use case. Run multiple benchmark iterations,
# recovering to pristine state between each.
###########################################################################
story "STORY 4: Multi-iteration bench loop (×3)"

teardown /tmp/s4
setup_volumes /tmp/s4

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

for i in 1 2 3; do
    assert_ok "mount (iter $i)" schelk mount
    is_mounted "$MP" || fail "mount live (iter $i)" "not in /proc/mounts"
    echo "iter_${i}" > "$MP/iter_${i}.txt"
    dd if=/dev/urandom of="$MP/wl_${i}.bin" bs=4k count=$((i * 25)) 2>/dev/null
    sync
    assert_ok "recover (iter $i)" schelk recover
done

assert_ok "final mount" schelk mount
is_mounted "$MP" || fail "final mount" "not mounted"
found=$(ls "$MP/" 2>/dev/null | grep -c "iter_")
if [ "$found" -eq 0 ]; then
    pass "3 iterations recovered cleanly"
else
    fail "multi-iteration" "$found leftover files"
fi
teardown /tmp/s4

###########################################################################
# Story 5: Promote — make scratch the new baseline
#
# From SPEC.md promote: after a schema migration or data load, the user
# wants the modified scratch to become the new virgin for future runs.
###########################################################################
story "STORY 5: Promote (scratch becomes new virgin)"

teardown /tmp/s5
setup_volumes /tmp/s5

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

assert_ok "mount" schelk mount
echo "schema_v2" > "$MP/version.txt"
mkdir -p "$MP/migrations"
echo "ALTER TABLE users ADD email TEXT;" > "$MP/migrations/001.sql"
sync

assert_ok "promote" schelk promote -y

# Verify promoted data is the new baseline
assert_ok "mount after promote" schelk mount
is_mounted "$MP" || fail "mount after promote" "not mounted"
[ -f "$MP/version.txt" ] && [ -f "$MP/migrations/001.sql" ] && \
    pass "promoted data persists as baseline" || fail "promote baseline" "missing"

# Write more data on new baseline, then recover
echo "bench on v2" > "$MP/bench_v2.txt"
sync
assert_ok "recover after promote" schelk recover

# Promoted data should survive, new writes should not
assert_ok "remount" schelk mount
if [ -f "$MP/version.txt" ] && [ ! -f "$MP/bench_v2.txt" ]; then
    pass "baseline kept, new writes removed"
else
    fail "recover after promote" "bad file state"
fi
teardown /tmp/s5

###########################################################################
# Story 6: Full recover — nuke and restore scratch from virgin
#
# From SPEC.md full-recover: costly full block copy, used when scratch is
# corrupted or for initial setup. We verify the content is fully restored.
###########################################################################
story "STORY 6: Full recover"

teardown /tmp/s6
setup_volumes /tmp/s6

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

# Mount, write a known marker, recover to establish clean baseline
assert_ok "mount" schelk mount
echo "baseline marker" > "$MP/marker.txt"
sync
# Promote so marker becomes part of virgin
assert_ok "promote marker" schelk promote -y

# Corrupt scratch directly
dd if=/dev/urandom of="$SCRATCH" bs=1M count=1 seek=5 conv=notrunc 2>/dev/null

assert_ok "full-recover" schelk full-recover -y

assert_ok "mount after full-recover" schelk mount
is_mounted "$MP" || fail "mount after full-recover" "not mounted"

# Verify the known marker is back (proves content was restored, not just mountable)
content=$(cat "$MP/marker.txt" 2>/dev/null)
if [ "$content" = "baseline marker" ]; then
    pass "full-recover restored content correctly"
else
    fail "full-recover content" "expected 'baseline marker', got '$content'"
fi
teardown /tmp/s6

###########################################################################
# Story 7: Status reporting at every lifecycle point
#
# From SPEC.md status: reports current state. We verify it correctly
# reflects: uninitialized, initialized/idle, mounted/tracking, recovered.
###########################################################################
story "STORY 7: Status at every lifecycle point"

teardown /tmp/s7
rm -f /var/lib/schelk/state.json

STATUS=$(schelk status 2>&1)
echo "$STATUS" | grep -q "not initialized" && \
    pass "status: not initialized" || fail "status uninit" "wrong output"

setup_volumes /tmp/s7
assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

STATUS=$(schelk status 2>&1)
echo "$STATUS" | grep -q "Mounted: no (state)" && \
echo "$STATUS" | grep -q "dm-era device: none" && \
    pass "status: initialized, not mounted" || fail "status init" "wrong output"

assert_ok "mount" schelk mount
STATUS=$(schelk status 2>&1)
echo "$STATUS" | grep -q "Mounted: yes (state)" && \
echo "$STATUS" | grep -q "Mounted: yes (actual)" && \
echo "$STATUS" | grep -q "dm-era device: active" && \
    pass "status: mounted and tracking" || fail "status mounted" "wrong output"

assert_ok "recover" schelk recover
STATUS=$(schelk status 2>&1)
echo "$STATUS" | grep -q "Mounted: no (state)" && \
echo "$STATUS" | grep -q "dm-era device: none" && \
    pass "status: after recover" || fail "status recovered" "wrong output"

teardown /tmp/s7

###########################################################################
# Story 8: Crash recovery — detect and handle inconsistent state
#
# From SPEC.md (mindset item 5): the app should prepare for crash of the
# system or the app itself. We simulate a crash by leaving dm-era and the
# mount live while tampering the state file to say "unmounted". Then we
# verify that status detects the inconsistency and mount refuses.
###########################################################################
story "STORY 8: Crash recovery (inconsistent state)"

teardown /tmp/s8
setup_volumes /tmp/s8

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y
assert_ok "mount" schelk mount
echo "in-flight" > "$MP/inflight.txt"
sync

# Simulate crash: flip state to unmounted while everything is still live
sed 's/"is_mounted": true/"is_mounted": false/' /var/lib/schelk/state.json | \
    sed 's/"current_era": 1/"current_era": null/' > /tmp/crash_state.json
cp /tmp/crash_state.json /var/lib/schelk/state.json

# Status should detect the mismatch: state says no, reality says yes
STATUS=$(schelk status 2>&1)
if echo "$STATUS" | grep -q "Mounted: no (state)" && \
   echo "$STATUS" | grep -q "YES (actual)" && \
   echo "$STATUS" | grep -q "EXISTS"; then
    pass "status detects crash inconsistency"
else
    fail "status crash" "did not flag mismatch"
fi

# Mount should refuse with non-zero exit
assert_fail "mount refuses with stale state" schelk mount

# Manual cleanup as instructed by error message
umount "$MP" 2>/dev/null
dmsetup remove bench_era 2>/dev/null

# After cleanup + full-recover, system should work again
assert_ok "full-recover after crash" schelk full-recover -y
assert_ok "mount after crash cleanup" schelk mount
is_mounted "$MP" || fail "mount after crash cleanup" "not mounted"

teardown /tmp/s8

###########################################################################
# Story 9: Reinitialize — init-new when already initialized
#
# From SPEC.md init-new: "If the app state already exists, it offers if
# it should reinitialize." Verify that re-running init-new with -y creates
# a fresh filesystem and the full cycle still works.
###########################################################################
story "STORY 9: Reinitialize"

teardown /tmp/s9
setup_volumes /tmp/s9

assert_ok "first init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

# Use it: mount, write a marker, promote so it's in the virgin baseline
assert_ok "mount" schelk mount
echo "old baseline" > "$MP/old_marker.txt"
sync
assert_ok "promote" schelk promote -y

# Reinitialize (second init-new with -y should wipe everything)
assert_ok "reinitialize (second init-new)" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

# The old promoted marker should be gone (fresh filesystem)
assert_ok "mount after reinit" schelk mount
is_mounted "$MP" || fail "mount after reinit" "not mounted"
if [ ! -f "$MP/old_marker.txt" ]; then
    pass "reinit wiped old baseline"
else
    fail "reinit wipe" "old_marker.txt still exists"
fi

# Full cycle works after reinit
echo "after reinit" > "$MP/reinit.txt"
sync
assert_ok "recover after reinit" schelk recover
assert_ok "remount" schelk mount
[ ! -f "$MP/reinit.txt" ] && \
    pass "full cycle works after reinit" || fail "cycle after reinit" "file survived"
teardown /tmp/s9

###########################################################################
# Story 10: Parallel instances — two independent schelk on separate volumes
#
# From SPEC.md: "Override with --dm-era-name to run multiple schelk
# instances in parallel (each must use a unique name, separate state files,
# and separate volumes/ramdisks)."
###########################################################################
story "STORY 10: Parallel instances"

teardown /tmp/s10a /tmp/s10b

# Instance A
mkdir -p /tmp/s10a
dd if=/dev/zero of=/tmp/s10a/virgin.img bs=1M count=32 2>/dev/null
dd if=/dev/zero of=/tmp/s10a/scratch.img bs=1M count=32 2>/dev/null
dd if=/dev/zero of=/tmp/s10a/ramdisk.img bs=1M count=4 2>/dev/null
A_VIRGIN=$(/sbin/losetup --find --show /tmp/s10a/virgin.img)
A_SCRATCH=$(/sbin/losetup --find --show /tmp/s10a/scratch.img)
A_RAMDISK=$(/sbin/losetup --find --show /tmp/s10a/ramdisk.img)
mkdir -p /tmp/mnt_a /tmp/state_a

# Instance B
mkdir -p /tmp/s10b
dd if=/dev/zero of=/tmp/s10b/virgin.img bs=1M count=32 2>/dev/null
dd if=/dev/zero of=/tmp/s10b/scratch.img bs=1M count=32 2>/dev/null
dd if=/dev/zero of=/tmp/s10b/ramdisk.img bs=1M count=4 2>/dev/null
B_VIRGIN=$(/sbin/losetup --find --show /tmp/s10b/virgin.img)
B_SCRATCH=$(/sbin/losetup --find --show /tmp/s10b/scratch.img)
B_RAMDISK=$(/sbin/losetup --find --show /tmp/s10b/ramdisk.img)
mkdir -p /tmp/mnt_b /tmp/state_b

assert_ok "init instance A" schelk init-new \
    --virgin "$A_VIRGIN" --scratch "$A_SCRATCH" --ramdisk "$A_RAMDISK" \
    --mount-point /tmp/mnt_a --dm-era-name era_a \
    --state-path /tmp/state_a/state.json -y

assert_ok "init instance B" schelk init-new \
    --virgin "$B_VIRGIN" --scratch "$B_SCRATCH" --ramdisk "$B_RAMDISK" \
    --mount-point /tmp/mnt_b --dm-era-name era_b \
    --state-path /tmp/state_b/state.json -y

assert_ok "mount A" schelk mount --state-path /tmp/state_a/state.json
assert_ok "mount B" schelk mount --state-path /tmp/state_b/state.json

is_mounted /tmp/mnt_a && is_mounted /tmp/mnt_b && \
    pass "both instances mounted simultaneously" || fail "parallel mount" "failed"

dmsetup status era_a >/dev/null 2>&1 && \
dmsetup status era_b >/dev/null 2>&1 && \
    pass "both dm-era devices coexist" || fail "dm-era coexist" "device missing"

echo "instance A" > /tmp/mnt_a/a.txt
echo "instance B" > /tmp/mnt_b/b.txt
sync

# Recover A while B stays mounted
assert_ok "recover A" schelk recover --state-path /tmp/state_a/state.json
assert_ok "remount A" schelk mount --state-path /tmp/state_a/state.json

if [ ! -f /tmp/mnt_a/a.txt ] && [ -f /tmp/mnt_b/b.txt ]; then
    pass "independent recovery (A recovered, B untouched)"
else
    fail "independent recovery" "A:a.txt=$([ -f /tmp/mnt_a/a.txt ] && echo yes || echo no) B:b.txt=$([ -f /tmp/mnt_b/b.txt ] && echo yes || echo no)"
fi

# Cleanup
schelk recover --state-path /tmp/state_a/state.json -y 2>&1 >/dev/null
schelk recover --state-path /tmp/state_b/state.json 2>&1 >/dev/null
teardown /tmp/s10a /tmp/s10b

###########################################################################
# Story 11: Full recover with stale mounted state (simulated reboot)
#
# After a reboot the state file still says "mounted" but the dm-era
# device and filesystem mount are gone.  full-recover should detect this
# stale state, auto-clear it, and proceed with the copy.
###########################################################################
story "STORY 11: Full recover with stale mounted state (simulated reboot)"

teardown /tmp/s11
setup_volumes /tmp/s11

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

assert_ok "mount" schelk mount
echo "pre-reboot data" > "$MP/pre_reboot.txt"
sync

# Simulate reboot: tear down dm-era and unmount, but leave state as-is.
# After this, state says is_mounted=true but nothing is actually live.
umount "$MP" 2>/dev/null
dmsetup remove bench_era 2>/dev/null

# full-recover should detect the stale state and proceed
assert_ok "full-recover with stale state" schelk full-recover -y

# Verify system is usable afterward
assert_ok "mount after stale full-recover" schelk mount
is_mounted "$MP" || fail "mount after stale full-recover" "not mounted"

# Data should be gone — we never promoted, so virgin has no pre_reboot.txt
if [ ! -f "$MP/pre_reboot.txt" ]; then
    pass "stale full-recover restored virgin correctly"
else
    fail "stale full-recover content" "pre_reboot.txt survived"
fi

assert_ok "recover (cleanup)" schelk recover

# Edge case: if dm-era device is still live, full-recover must reject.
# Simulate by mounting normally then only unmounting the filesystem.
assert_ok "remount for edge case" schelk mount
umount "$MP" 2>/dev/null
# dm-era device is still live — full-recover should refuse
assert_fail "full-recover rejects when dm-era still exists" schelk full-recover -y

# Clean up the live dm-era device
dmsetup remove bench_era 2>/dev/null
teardown /tmp/s11

###########################################################################
# Story 12: Recover is a no-op when not mounted
#
# From user-stories.md story 18: recover should exit 0 when the volume
# is not mounted.  This matters for CI scripts that run
# `schelk recover || schelk full-recover` — a non-zero exit from
# recover when nothing is mounted would trigger an unnecessary
# full-recover.
###########################################################################
story "STORY 12: Recover is a no-op when not mounted"

teardown /tmp/s12
setup_volumes /tmp/s12

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

# Never mounted — recover should succeed (no-op)
assert_ok "recover when never mounted" schelk recover

# Mount, recover normally, then recover again — second should be no-op
assert_ok "mount" schelk mount
echo "data" > "$MP/data.txt"
sync
assert_ok "recover (normal)" schelk recover
assert_ok "recover again (already recovered)" schelk recover

teardown /tmp/s12

###########################################################################
# Story 13: O_EXCL refuses to write to a volume mounted outside schelk
#
# From user-stories.md story 20 and SPEC.md principles 3 and 11
# ("foolproof", "principle of least surprise"). If the user mounts virgin
# or scratch outside of schelk, any subsequent destructive write would
# silently corrupt the live filesystem. Opening the raw device with
# O_EXCL lets the kernel reject the operation with EBUSY before any
# block is written.
###########################################################################
story "STORY 13: Refuse writes to volumes mounted outside schelk"

teardown /tmp/s13
setup_volumes /tmp/s13

assert_ok "init-new" schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MP" -y

# Mount scratch directly, outside schelk's control. After init-new the
# scratch volume is byte-identical to virgin and carries a valid ext4 fs.
mkdir -p /tmp/s13_external
mount -t ext4 "$SCRATCH" /tmp/s13_external

# full-recover writes to scratch. With O_EXCL on the destination open,
# the kernel must reject it because scratch is currently mounted.
assert_fail "full-recover refuses when scratch is mounted externally" \
    schelk full-recover -y

# Error message should clearly explain that the device is in use.
echo "$LAST_OUT" | grep -qi "in use" && \
    pass "error mentions 'in use'" || \
    fail "error message" "did not mention 'in use'"

# Unmounting the external mount must let full-recover succeed.
umount /tmp/s13_external

assert_ok "full-recover succeeds after external unmount" \
    schelk full-recover -y

umount /tmp/s13_external 2>/dev/null || true
teardown /tmp/s13 /tmp/s13_external

###########################################################################
# Results
###########################################################################

echo ""
echo "============================================"
echo "Test Results"
echo "============================================"
echo "  Passed: $PASSED"
echo "  Failed: $FAILED"
if [ -n "$ERRORS" ]; then
    echo ""
    echo "Failures:"
    printf "$ERRORS\n"
fi
echo ""

if [ "$FAILED" -eq 0 ]; then
    echo "SCHELK_TEST_RESULT=PASS"
else
    echo "SCHELK_TEST_RESULT=FAIL"
fi

exec poweroff -f
