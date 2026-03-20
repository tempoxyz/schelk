#!/bin/sh
# schelk QEMU integration test runner.
#
# This script runs as PID 1 (/init) inside a minimal QEMU VM. It exercises
# the full schelk lifecycle: module loading, dm-era operations, init-new,
# mount, write, recover, and verification.
#
# Results go to serial console (ttyS0). The host script (ci/run-vm-tests.sh)
# looks for SCHELK_TEST_RESULT=PASS/FAIL to determine success.

set -u

export PATH=/bin:/sbin:/usr/sbin:/usr/bin
export HOME=/tmp

# Mount essential filesystems
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
mkdir -p /dev/mapper

# Without udev, /dev/mapper/ entries aren't auto-created.
# Disable udev sync and wrap dmsetup so create/remove also calls mknodes.
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

echo "============================================"
echo "schelk QEMU integration test runner"
echo "============================================"
echo ""

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

# ── Load kernel modules ──────────────────────────────────────────────

load_mod() {
    local name="$1"
    shift
    # Already loaded?
    if [ -d "/sys/module/${name}" ] || [ -d "/sys/module/$(echo "$name" | tr '-' '_')" ]; then
        echo "  $name: already loaded"
        return 0
    fi
    local mod_file
    mod_file=$(find /lib/modules -name "${name}.ko*" 2>/dev/null | head -1)
    if [ -z "$mod_file" ]; then
        echo "  $name: not found (likely built-in)"
        return 0
    fi
    local decompressed="/tmp/${name}.ko"
    case "$mod_file" in
        *.zst) zstd -d -q "$mod_file" -o "$decompressed" 2>/dev/null ;;
        *.xz)  xz -d -q -k "$mod_file" -c > "$decompressed" 2>/dev/null ;;
        *.gz)  gzip -d -q -k "$mod_file" -c > "$decompressed" 2>/dev/null ;;
        *.ko)  decompressed="$mod_file" ;;
    esac
    insmod "$decompressed" "$@" 2>&1 && echo "  $name: loaded" || echo "  $name: FAILED"
}

echo "--- Loading kernel modules ---"
# Load in dependency order. Many of these are built-in on Ubuntu but may
# be loadable on other kernels/distros.
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

echo ""
echo "============================================"
echo "Running tests..."
echo "============================================"

# ── Test 1: Prerequisites ─────────────────────────────────────────────

echo ""
echo "--- Test: prerequisites ---"

if schelk --help > /dev/null 2>&1; then
    pass "schelk binary"
else
    fail "schelk binary" "failed to execute"
fi

if dmsetup version > /dev/null 2>&1; then
    pass "dmsetup"
else
    fail "dmsetup" "not functional"
fi

if dmsetup targets 2>/dev/null | grep -q "era"; then
    pass "dm-era target"
else
    fail "dm-era target" "era not available in device-mapper"
fi

# ── Test 2: dm-era lifecycle (manual) ─────────────────────────────────

echo ""
echo "--- Test: dm-era create/checkpoint/snapshot/remove ---"
TEST_DIR="/tmp/schelk-test"
mkdir -p "$TEST_DIR"

dd if=/dev/zero of="$TEST_DIR/origin.img" bs=1M count=10 2>/dev/null
dd if=/dev/zero of="$TEST_DIR/metadata.img" bs=1M count=2 2>/dev/null

ORIGIN_LOOP=$(/sbin/losetup --find --show "$TEST_DIR/origin.img")
META_LOOP=$(/sbin/losetup --find --show "$TEST_DIR/metadata.img")

if [ -b "$ORIGIN_LOOP" ] && [ -b "$META_LOOP" ]; then
    pass "loop devices ($ORIGIN_LOOP $META_LOOP)"

    # 10MB = 20480 sectors, 4K granularity = 8 sectors
    TABLE="0 20480 era $META_LOOP $ORIGIN_LOOP 8"
    if dmsetup create schelk_test_era --table "$TABLE" 2>&1; then
        pass "dm-era create"
        dmsetup mknodes 2>/dev/null

        dmsetup message schelk_test_era 0 checkpoint 2>/dev/null && \
            pass "dm-era checkpoint" || fail "dm-era checkpoint" "failed"

        [ -b /dev/mapper/schelk_test_era ] && \
            pass "dm-era /dev/mapper node" || fail "dm-era /dev/mapper" "node missing"

        if dmsetup message schelk_test_era 0 take_metadata_snap 2>/dev/null; then
            pass "take_metadata_snap"
            if era_invalidate --metadata-snapshot --written-since 0 "$META_LOOP" > /dev/null 2>&1; then
                pass "era_invalidate"
            else
                fail "era_invalidate" "failed"
            fi
            dmsetup message schelk_test_era 0 drop_metadata_snap 2>/dev/null
        else
            fail "take_metadata_snap" "failed"
        fi

        dmsetup remove schelk_test_era 2>/dev/null && \
            pass "dm-era remove" || fail "dm-era remove" "failed"
    else
        fail "dm-era create" "failed"
    fi

    /sbin/losetup -d "$ORIGIN_LOOP" 2>/dev/null
    /sbin/losetup -d "$META_LOOP" 2>/dev/null
else
    fail "loop devices" "failed to create"
fi

# ── Test 3: Full schelk lifecycle ─────────────────────────────────────

echo ""
echo "--- Test: schelk init-new -> mount -> recover ---"

dd if=/dev/zero of="$TEST_DIR/virgin.img" bs=1M count=32 2>/dev/null
dd if=/dev/zero of="$TEST_DIR/scratch.img" bs=1M count=32 2>/dev/null
dd if=/dev/zero of="$TEST_DIR/ramdisk.img" bs=1M count=4 2>/dev/null

VIRGIN=$(/sbin/losetup --find --show "$TEST_DIR/virgin.img")
SCRATCH=$(/sbin/losetup --find --show "$TEST_DIR/scratch.img")
RAMDISK=$(/sbin/losetup --find --show "$TEST_DIR/ramdisk.img")

MOUNT_POINT="/tmp/schelk-mount"
mkdir -p "$MOUNT_POINT"

if schelk init-new \
    --virgin "$VIRGIN" --scratch "$SCRATCH" --ramdisk "$RAMDISK" \
    --mount-point "$MOUNT_POINT" -y 2>&1; then
    pass "schelk init-new"

    if schelk mount 2>&1; then
        pass "schelk mount"

        echo "hello from schelk QEMU test" > "$MOUNT_POINT/testfile.txt" 2>/dev/null
        sync
        [ -f "$MOUNT_POINT/testfile.txt" ] && \
            pass "write to mounted fs" || fail "write to fs" "file not created"

        if schelk recover 2>&1; then
            pass "schelk recover"

            # Remount and verify the file is gone
            if schelk mount 2>&1; then
                if [ ! -f "$MOUNT_POINT/testfile.txt" ]; then
                    pass "recovery correctness (file removed)"
                else
                    fail "recovery correctness" "file still exists"
                fi
                schelk recover -y 2>&1 || true
            else
                fail "remount after recover" "mount failed"
            fi
        else
            fail "schelk recover" "failed"
        fi
    else
        fail "schelk mount" "failed"
    fi
else
    fail "schelk init-new" "failed"
fi

/sbin/losetup -d "$VIRGIN" 2>/dev/null || true
/sbin/losetup -d "$SCRATCH" 2>/dev/null || true
/sbin/losetup -d "$RAMDISK" 2>/dev/null || true

# ── Results ───────────────────────────────────────────────────────────

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

# Power off
echo ""
exec /bin/sh -c "echo o > /proc/sysrq-trigger"
