#!/bin/bash
# Build a minimal initramfs and boot it in QEMU to run schelk integration tests.
#
# This script handles the entire host-side orchestration:
#   1. Detect the installed kernel version
#   2. Collect kernel modules (dm-era, brd, and deps)
#   3. Bundle schelk + system tools into a cpio initramfs
#   4. Create scratch disk images
#   5. Boot QEMU (KVM if available, TCG otherwise)
#   6. Parse serial output for SCHELK_TEST_RESULT=PASS/FAIL
#
# Usage:
#   ci/run-vm-tests.sh [path-to-schelk-binary]
#
# Requirements:
#   - qemu-system-x86_64
#   - A linux kernel in /boot/vmlinuz-*
#   - dmsetup, era_invalidate, mkfs.ext4, losetup, busybox, zstd

set -euo pipefail

SCHELK_BIN="${1:-target/release/schelk}"
WORK_DIR=$(mktemp -d /tmp/schelk-vm-test.XXXXXX)
INITRAMFS_DIR="$WORK_DIR/initramfs"
TIMEOUT="${QEMU_TIMEOUT:-180}"

# ── Detect kernel ─────────────────────────────────────────────────────

VMLINUZ=$(ls /boot/vmlinuz-* 2>/dev/null | sort -V | tail -1)
if [ -z "$VMLINUZ" ]; then
    echo "ERROR: No kernel found in /boot/vmlinuz-*"
    echo "Install one: sudo apt-get install linux-image-generic"
    exit 1
fi
KVER=$(basename "$VMLINUZ" | sed 's/vmlinuz-//')
echo "=== schelk QEMU integration tests ==="
echo "  Kernel: $KVER ($VMLINUZ)"
echo "  schelk: $SCHELK_BIN"
echo "  Timeout: ${TIMEOUT}s"
echo ""

if [ ! -f "$SCHELK_BIN" ]; then
    echo "ERROR: schelk binary not found at $SCHELK_BIN"
    echo "Build first: cargo build --release"
    exit 1
fi

# ── Build initramfs ───────────────────────────────────────────────────

cleanup() {
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

mkdir -p "$INITRAMFS_DIR"/{bin,sbin,lib,lib64,proc,sys,dev,tmp,var/lib/schelk,usr/sbin}
mkdir -p "$INITRAMFS_DIR/lib/modules/$KVER"

# Busybox
BUSYBOX=$(command -v busybox) || { echo "ERROR: busybox not found"; exit 1; }
cp "$BUSYBOX" "$INITRAMFS_DIR/bin/busybox"
for cmd in sh echo cat ls mount umount mkdir sleep mknod dd sync modprobe \
           insmod lsmod depmod grep head tail wc tr basename test true false \
           printf rm cp mv chmod chown ln which find; do
    ln -sf busybox "$INITRAMFS_DIR/bin/$cmd"
done

# Kernel modules — copy everything in the dm-era dependency chain + extras.
# On many Ubuntu kernels dm-mod/ext4/loop are built-in, but we copy them if
# present so this works on kernels where they're loadable too.
MODULE_NAMES="dm-era brd dm-persistent-data dm-bufio libcrc32c dm-mod loop ext4 jbd2 crc16 mbcache crc32c_generic"
for name in $MODULE_NAMES; do
    while IFS= read -r mod; do
        [ -f "$mod" ] || continue
        rel="${mod#/lib/modules/$KVER/}"
        dest_dir="$INITRAMFS_DIR/lib/modules/$KVER/$(dirname "$rel")"
        mkdir -p "$dest_dir"
        cp "$mod" "$dest_dir/"
    done < <(find "/lib/modules/$KVER" -name "${name}.ko*" 2>/dev/null)
done

# Verify dm-era module was found
find "$INITRAMFS_DIR/lib/modules" -name "dm-era.ko*" | grep -q . || {
    echo "ERROR: dm-era kernel module not found in /lib/modules/$KVER"
    exit 1
}

# Binaries
cp "$SCHELK_BIN" "$INITRAMFS_DIR/bin/schelk"
cp "$(which dmsetup)" "$INITRAMFS_DIR/sbin/dmsetup"
cp "$(which mkfs.ext4)" "$INITRAMFS_DIR/sbin/mkfs.ext4"
cp "$(which losetup)" "$INITRAMFS_DIR/sbin/losetup"
cp "$(which zstd)" "$INITRAMFS_DIR/bin/zstd" 2>/dev/null || true

# era_invalidate (may be a standalone binary or a pdata_tools multi-call)
if which era_invalidate >/dev/null 2>&1; then
    cp "$(which era_invalidate)" "$INITRAMFS_DIR/sbin/era_invalidate"
elif which pdata_tools >/dev/null 2>&1; then
    cp "$(which pdata_tools)" "$INITRAMFS_DIR/sbin/era_invalidate"
else
    echo "ERROR: era_invalidate not found"
    exit 1
fi

# Shared libraries
copy_libs() {
    for lib in $(ldd "$1" 2>/dev/null | grep -o '/[^ ]*' | sort -u); do
        [ -f "$lib" ] || continue
        dest="$INITRAMFS_DIR$lib"
        mkdir -p "$(dirname "$dest")"
        cp -n "$lib" "$dest" 2>/dev/null || true
    done
}

for bin in "$INITRAMFS_DIR"/bin/schelk "$INITRAMFS_DIR"/sbin/dmsetup \
           "$INITRAMFS_DIR"/sbin/era_invalidate "$INITRAMFS_DIR"/sbin/mkfs.ext4 \
           "$INITRAMFS_DIR"/sbin/losetup "$INITRAMFS_DIR"/bin/zstd; do
    [ -f "$bin" ] && copy_libs "$bin"
done

# Dynamic linker
for ld in /lib64/ld-linux-x86-64.so.* /lib/x86_64-linux-gnu/ld-linux-x86-64.so.*; do
    if [ -f "$ld" ]; then
        dest="$INITRAMFS_DIR$ld"
        mkdir -p "$(dirname "$dest")"
        cp -n "$ld" "$dest" 2>/dev/null || true
    fi
done

# /etc/mke2fs.conf (mkfs.ext4 may need it)
[ -f /etc/mke2fs.conf ] && mkdir -p "$INITRAMFS_DIR/etc" && cp /etc/mke2fs.conf "$INITRAMFS_DIR/etc/"

# /init script
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cp "$SCRIPT_DIR/vm-init.sh" "$INITRAMFS_DIR/init"
chmod +x "$INITRAMFS_DIR/init"

# Pack
INITRAMFS="$WORK_DIR/initramfs.cpio.gz"
(cd "$INITRAMFS_DIR" && find . -print0 | cpio --null -o --format=newc 2>/dev/null | gzip -9) > "$INITRAMFS"

echo "  initramfs: $(du -h "$INITRAMFS" | cut -f1)"

# ── Boot QEMU ─────────────────────────────────────────────────────────

ACCEL="tcg"
if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL="kvm"
    echo "  Accelerator: KVM"
else
    echo "  Accelerator: TCG (no KVM — slower but functional)"
fi

OUTPUT="$WORK_DIR/serial.log"

echo ""
echo "Booting QEMU..."
echo ""

set +e
timeout "$TIMEOUT" qemu-system-x86_64 \
    -machine "accel=$ACCEL" \
    -cpu max \
    -smp 2 \
    -m 1024 \
    -display none \
    -no-reboot \
    -kernel "$VMLINUZ" \
    -initrd "$INITRAMFS" \
    -append "console=ttyS0 panic=1 quiet" \
    -serial mon:stdio \
    2>&1 | tee "$OUTPUT"
qemu_rc=${PIPESTATUS[0]}
set -e

# ── Parse results ─────────────────────────────────────────────────────

echo ""

if [ "$qemu_rc" -eq 124 ]; then
    echo "=== QEMU integration tests TIMED OUT (${TIMEOUT}s) ==="
    exit 1
fi

if grep -q "SCHELK_TEST_RESULT=PASS" "$OUTPUT"; then
    echo "=== QEMU integration tests PASSED ==="
    exit 0
elif grep -q "SCHELK_TEST_RESULT=FAIL" "$OUTPUT"; then
    echo "=== QEMU integration tests FAILED ==="
    exit 1
else
    echo "=== QEMU integration tests: no result marker found (QEMU exit code: $qemu_rc) ==="
    exit 1
fi
