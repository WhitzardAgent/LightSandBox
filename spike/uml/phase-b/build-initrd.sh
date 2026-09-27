#!/usr/bin/env bash
# Shared initramfs builder for T47 phase-b/phase-c. Sources nothing; expects
# SPIKE, WORK, PB exported by the caller. Rebuilt on every call: a stale cpio
# silently running an OLD guest-init is a failure mode we already hit once
# (T47-C02 first attempt fell through to LSB_PHASE_UNKNOWN_MODE).
set -euo pipefail

[ -n "${SPIKE:-}" ] && [ -n "${WORK:-}" ] && [ -n "${PB:-}" ] || {
  echo "build-initrd.sh: SPIKE/WORK/PB must be set" >&2; exit 1; }

BUSYBOX="$WORK/rootfs/bin/busybox"
UMOCI="$WORK/tools/umoci-static"
GEN="$WORK/bin/gen_init_cpio"
CPIO="$PB/phase-b.cpio"
APPS="sh cat echo mkdir mknod mount umount awk sed tr cut touch rm ls stat su id env poweroff sleep sync sha256sum readlink chroot dd"

mkdir -p "$PB/initrd/bin" "$PB/initrd/tools"
cp "$BUSYBOX" "$PB/initrd/bin/busybox"
cp "$UMOCI" "$PB/initrd/tools/umoci-static"
cp "$SPIKE/guest-init.sh" "$PB/initrd/init"
chmod 755 "$PB/initrd/init"
for app in $APPS; do
  ln -sf busybox "$PB/initrd/bin/$app"
done
{
  printf 'dir / 755 0 0\ndir /bin 755 0 0\ndir /tools 755 0 0\ndir /oci 755 0 0\ndir /target 755 0 0\n'
  printf 'file /init %s 755 0 0\n' "$PB/initrd/init"
  printf 'file /bin/busybox %s 755 0 0\n' "$PB/initrd/bin/busybox"
  printf 'file /tools/umoci-static %s 755 0 0\n' "$PB/initrd/tools/umoci-static"
  for app in $APPS; do
    printf 'slink /bin/%s busybox 777 0 0\n' "$app"
  done
  printf 'nod /dev/console 600 0 0 c 5 1\nnod /dev/null 666 0 0 c 1 3\nnod /dev/tty 666 0 0 c 5 0\n'
} | "$GEN" -t 0 - > "$CPIO"
echo "phase-b.cpio rebuilt from guest-init.sh @ $(date -u +%H:%M:%S)"
