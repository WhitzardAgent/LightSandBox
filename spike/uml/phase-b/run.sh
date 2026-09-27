#!/usr/bin/env bash
# T47 Phase-B orchestrator (host side). Boots the pinned UML kernel with an
# external initramfs (initrd= override proven in this session: external /init
# wins over the embedded phase-a one, so the phase-a kernel binary stays
# untouched as the pinned artifact), materializes the OCI fixture into a
# fresh ext4 UBD disk via guest-side umoci, reconciles in-guest stat truth,
# then boots the base disk and runs the config-derived process.
set -uo pipefail

SPIKE="$(cd "$(dirname "$0")" && pwd)"
UMLROOT="$(cd "$SPIKE/.." && pwd)"
WORK="$UMLROOT/work"
PB="$WORK/phase-b"
KERNEL="$WORK/src/linux/linux"
BUSYBOX="$WORK/rootfs/bin/busybox"
UMOCI="$WORK/tools/umoci-static"
GEN="$WORK/bin/gen_init_cpio"
PY=/usr/bin/python3.12
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/sbin:/sbin:/usr/bin:/bin:$PATH"

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

[ -x "$KERNEL" ] || { echo "pinned UML kernel missing: $KERNEL"; exit 1; }
[ -x "$UMOCI" ] || { echo "static umoci missing: $UMOCI"; exit 1; }
KSHA="$(sha256sum "$KERNEL" | awk '{print $1}')"
log "kernel sha256 $KSHA"

# ---- 1. fixture -------------------------------------------------------------
"$PY" "$SPIKE/build_fixture.py"

# ---- 2. blank target disk (regular file; formatted host-side) ---------------
DISK="$PB/base-disk.img"
if [ ! -f "$DISK" ]; then
  dd if=/dev/zero of="$DISK" bs=1M count=96 status=none
  "$PY" - "$DISK" <<'PY'
import subprocess, sys
# regular FILE formatting (not a device node) — the standard way to make a
# UBD disk image; mkfs.ext4 lives at /usr/sbin on this host.
subprocess.run(["/usr/sbin/mkfs.ext4", "-q", "-F", sys.argv[1]], check=True)
PY
fi
log "base disk ready: $(ls -la "$DISK" | awk '{print $5" bytes"}')"

# ---- 3. initramfs with busybox + static umoci -------------------------------
CPIO="$PB/phase-b.cpio"
export SPIKE WORK PB
bash "$SPIKE/build-initrd.sh"
log "initramfs: $(stat -c %s "$CPIO") bytes"

# ---- 4. BUILD boot ----------------------------------------------------------
OCI_HOST="$PB/oci-cache"
CONSOLE_BUILD="$PB/console-build.log"
log "boot 1/2: materialize (timeout 300s)"
T0=$(date +%s.%N)
timeout 300 "$KERNEL" mem=256M con0=fd:0,fd:1 ubd0="$DISK" initrd="$CPIO" \
    lsbx.mode=build lsbx.ocipath="$OCI_HOST" </dev/null > "$CONSOLE_BUILD" 2>&1
RC_B=$?
T1=$(date +%s.%N)
log "build boot rc=$RC_B wall=$(awk "BEGIN{printf \"%.2f\", $T1-$T0}")s"
grep -E "LSB_(HOSTFS|UBD|UNPACK|RTCONFIG|PHASE|CONFIGSHA)" "$CONSOLE_BUILD" | head -20

# ---- 5. RUNTIME boot --------------------------------------------------------
CONSOLE_RT="$PB/console-runtime.log"
log "boot 2/2: runtime (timeout 300s)"
T0=$(date +%s.%N)
timeout 300 "$KERNEL" mem=256M con0=fd:0,fd:1 ubd0="$DISK" initrd="$CPIO" \
    lsbx.mode=runtime lsbx.ocipath="$OCI_HOST" </dev/null > "$CONSOLE_RT" 2>&1
RC_R=$?
T1=$(date +%s.%N)
log "runtime boot rc=$RC_R wall=$(awk "BEGIN{printf \"%.2f\", $T1-$T0}")s"
grep -E "LSB_RT|RT_CFG|SVC_|LSB_PHASE" "$CONSOLE_RT" | head -20

# ---- 6. reconciliation → uml-oci-fidelity.json -------------------------------
"$PY" "$SPIKE/reconcile.py" "$PB" "$KSHA" "$RC_B" "$RC_R"
