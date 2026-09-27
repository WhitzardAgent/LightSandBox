#!/usr/bin/env bash
# T47-C02: UBD COW correctness + cost, against the base disk built by run.sh.
# Legs A (write) / B (fresh cow, must NOT see A) / A2 (A's cow again, must see
# A) / C (fresh cow, clean). Base digest is sampled between every leg.
# UML COW syntax: ubd0=<cow-file>,<backing-file> (official UBD driver COW;
# we do NOT reimplement block COW).
set -uo pipefail

SPIKE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(cd "$SPIKE/../work" && pwd)"
PB="$WORK/phase-b"
KERNEL="$WORK/src/linux/linux"
CPIO="$PB/phase-b.cpio"
BASE="$PB/base-disk.img"
COWDIR="$PB/cow"
PY=/usr/bin/python3.12
mkdir -p "$COWDIR"

log() { echo "[$(date -u +%H:%M:%S)] $*"; }
basesha() { sha256sum "$BASE" | awk '{print $1}'; }

[ -f "$BASE" ] || { echo "base disk missing — run run.sh first"; exit 1; }
# Always rebuild the initramfs from the current guest-init.sh; a stale cpio
# boots an OLD init and silently mis-scores everything (learned the hard way).
export SPIKE WORK PB
bash "$SPIKE/build-initrd.sh"
[ -f "$CPIO" ] || { echo "initramfs build failed"; exit 1; }

clean_cows() {
  local f
  for f in "$COWDIR"/*.cow; do [ -e "$f" ] && "$PY" -c "import os,sys; os.unlink(sys.argv[1])" "$f"; done
}
clean_cows
SHA0="$(basesha)"
log "base sha256 @start: $SHA0"

boot_leg() { # <leg> <cowarg> <logfile>
  local leg="$1" cow="$2" con="$3" t0 t1
  t0=$(date +%s.%N)
  timeout 300 "$KERNEL" mem=256M con0=fd:0,fd:1 "ubd0=$cow" initrd="$CPIO" \
      lsbx.mode=cow "lsbx.leg=$leg" lsbx.ocipath="$PB/oci-cache" \
      </dev/null > "$con" 2>&1
  local rc=$?
  t1=$(date +%s.%N)
  log "leg $leg rc=$rc wall=$(awk "BEGIN{printf \"%.2f\", $t1-$t0}")s ($con)"
  grep -E "LSB_COW|LSB_PHASE" "$con" | head -6
}

alloc() { # <file> -> "%s %b*512"
  [ -f "$1" ] && stat -c '%s %b' "$1" | awk '{printf "%s %d", $1, $2*512}' || echo "0 0"
}

# --- leg A: writer ---
T0=$(date +%s.%N)
boot_leg A "$COWDIR/a.cow,$BASE" "$COWDIR/console-A.log"
T1=$(date +%s.%N)
CREATE_A_MS=$(awk "BEGIN{printf \"%d\", ($T1-$T0)*1000}")
SHA1="$(basesha)"; [ "$SHA1" = "$SHA0" ] || log "BASE SHA CHANGED AFTER A: $SHA1"
read SA BA < <(alloc "$COWDIR/a.cow") || true
log "a.cow size=$SA allocated=${BA}B after 1MB+100MB writes (create-wall ${CREATE_A_MS}ms)"

# --- leg B: fresh cow, must be isolated from A ---
boot_leg B "$COWDIR/b.cow,$BASE" "$COWDIR/console-B.log"
SHA2="$(basesha)"; [ "$SHA2" = "$SHA0" ] || log "BASE SHA CHANGED AFTER B: $SHA2"

# --- leg A2: A's cow again, must see A's writes ---
boot_leg A2 "$COWDIR/a.cow,$BASE" "$COWDIR/console-A2.log"
SHA3="$(basesha)"; [ "$SHA3" = "$SHA0" ] || log "BASE SHA CHANGED AFTER A2: $SHA3"

# --- leg C: another fresh cow ---
boot_leg C "$COWDIR/c.cow,$BASE" "$COWDIR/console-C.log"
SHA4="$(basesha)"; [ "$SHA4" = "$SHA0" ] || log "BASE SHA CHANGED AFTER C: $SHA4"

# --- cow delete latency ---
T0=$(date +%s.%N)
"$PY" -c "import os; os.unlink('$COWDIR/c.cow')"
T1=$(date +%s.%N)
DELETE_MS=$(awk "BEGIN{printf \"%d\", ($T1-$T0)*1000}")
read SB BB2 < <(alloc "$COWDIR/b.cow") || true

SHAS_EQUAL=true
[ "$SHA0" = "$SHA4" ] || SHAS_EQUAL=false

CREATE_EMPTY_MS=$( { T0=$(date +%s%N); : > "$COWDIR/empty.cow"; T1=$(date +%s%N); echo $(( (T1-T0)/1000000 )); } )
"$PY" -c "import os; os.path.exists('$COWDIR/empty.cow') and os.unlink('$COWDIR/empty.cow')"

log "assembling uml-ubd-cow.json"
"$PY" "$SPIKE/reconcile-cow.py" "$COWDIR" "$SHA0" "$SHAS_EQUAL" \
    "$CREATE_A_MS" "$DELETE_MS" "$CREATE_EMPTY_MS" "$SA" "$BA" "$SB" "$BB2"
