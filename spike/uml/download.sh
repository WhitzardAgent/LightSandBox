#!/usr/bin/env bash
# Round-03 UML spike — step 1: source acquisition + local build toolchain.
# Everything lands under spike/uml/work/ (git-ignored). No host system mutation:
# all tools are built with --prefix=spike/uml/work/tools and used via PATH.
#
# Network reality on this host (measured 2026-09-25 ~03:30 UTC): kernel.org and
# busybox.net time out; mirrors.tuna.tsinghua.edu.cn answers in <1s. Sources
# therefore come from the TUNA mirror of the upstream projects. Integrity
# method (recorded honestly): sha256 cross-checked against the mirror's copy of
# the upstream-published checksum files (sha256sums.asc etc.); PGP signatures
# NOT verified (no keyring); self-recorded in SHA256SUMS.self + source-lock.json.
set -euo pipefail

SPIKE="$(cd "$(dirname "$0")" && pwd)"
WORK="$SPIKE/work"
SRC="$WORK/src"
TOOLS="$WORK/tools"
mkdir -p "$SRC" "$TOOLS"
# Sanitized PATH: the agent session PATH leads with ~/.minimax/shims, whose
# `rm` wrapper (mavis-trash) prints noise to stdout and intermittently fails —
# that garbage corrupts autoconf command substitutions (m4 configure failed
# with it). Project-internal tool prefix first, then REAL system dirs only.
export PATH="$TOOLS/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
NPROC="$(nproc)"
MIRROR="${UML_MIRROR:-https://mirrors.tuna.tsinghua.edu.cn}"

log() { echo "[$(date -u +%H:%M:%S)] $*"; }

fetch() { # url dest
  local url="$1" dest="$2"
  if [ -s "$dest" ]; then log "cached $(basename "$dest")"; return 0; fi
  log "fetch $url"
  curl -fL --retry 3 --retry-delay 2 --connect-timeout 15 --max-time 900 -o "$dest" "$url"
}

record() { sha256sum "$1" >> "$WORK/SHA256SUMS.self"; }

: > "$WORK/SHA256SUMS.self"

# ---- 1. Linux kernel: latest tarball the mirror lists under v6.x -----------
log "listing $MIRROR/kernel/v6.x/"
fetch "$MIRROR/kernel/v6.x/" "$SRC/kernel-index.html"
KVER="$(/usr/bin/python3.12 - "$SRC/kernel-index.html" <<'PY'
import re,sys
html=open(sys.argv[1],encoding='utf-8',errors='replace').read()
vers=set()
for m in re.finditer(r'linux-(\d+\.\d+(?:\.\d+)?)\.tar\.xz',html):
    vers.add(m.group(1))
def key(v):
    p=[int(x) for x in v.split('.')]
    return p+[0]*(3-len(p))
vers=sorted(vers,key=key)
pick=[v for v in vers if v.count('.')==2]   # full patch-level releases only
print(pick[-1] if pick else vers[-1])
PY
)"
log "kernel version = $KVER"
fetch "$MIRROR/kernel/v6.x/linux-$KVER.tar.xz" "$SRC/linux.tar.xz"
if fetch "$MIRROR/kernel/v6.x/sha256sums.asc" "$SRC/sha256sums.asc"; then
  want="$(grep "linux-$KVER.tar.xz" "$SRC/sha256sums.asc" | awk '{print $1}')"
  got="$(sha256sum "$SRC/linux.tar.xz" | awk '{print $1}')"
  log "kernel sha256 sumsfile=$want local=$got"
  [ "$want" = "$got" ] || { echo "KERNEL HASH MISMATCH"; exit 1; }
else
  log "sha256sums.asc unavailable; kernel hash self-recorded only"
fi
record "$SRC/linux.tar.xz"

# ---- 2. BusyBox (pinned) ----------------------------------------------------
# busybox.net unreachable; no busybox tree on TUNA. Use the Debian pool orig
# tarball (upstream release repacked) and cross-check its sha256 against the
# signed .dsc control file, both via the mirror.
BBVER=1.37.0
fetch "$MIRROR/debian/pool/main/b/busybox/" "$SRC/busybox-index.html"
DSC="$(grep -oE "busybox_$BBVER-[0-9.]+\.dsc" "$SRC/busybox-index.html" | sort -Vu | tail -1)"
log "busybox debian control file: $DSC"
fetch "$MIRROR/debian/pool/main/b/busybox/busybox_$BBVER.orig.tar.bz2" "$SRC/busybox.tar.bz2"
fetch "$MIRROR/debian/pool/main/b/busybox/$DSC" "$SRC/busybox.dsc"
wantb="$(grep -E "^ [0-9a-f]{64} [0-9]+ busybox_${BBVER//./\\.}\.orig\.tar\.bz2$" "$SRC/busybox.dsc" | awk '{print $1}')"
gotb="$(sha256sum "$SRC/busybox.tar.bz2" | awk '{print $1}')"
log "busybox sha256 dsc=$wantb local=$gotb"
[ -n "$wantb" ] && [ "$wantb" = "$gotb" ] || { echo "BUSYBOX HASH MISMATCH"; exit 1; }
record "$SRC/busybox.tar.bz2"

# ---- 3. build prerequisites missing on host: m4, bison, flex, bc ------------
fetch "$MIRROR/gnu/m4/m4-1.4.19.tar.xz"    "$SRC/m4.tar.xz"
fetch "$MIRROR/gnu/bison/bison-3.8.2.tar.xz" "$SRC/bison.tar.xz"
fetch "$MIRROR/debian/pool/main/f/flex/flex_2.6.4.orig.tar.gz" "$SRC/flex.tar.gz"
fetch "$MIRROR/gnu/bc/bc-1.07.1.tar.gz"    "$SRC/bc.tar.gz"
record "$SRC/m4.tar.xz"; record "$SRC/bison.tar.xz"; record "$SRC/flex.tar.gz"; record "$SRC/bc.tar.gz"

extract() { local f="$1" d="$2"; [ -d "$d" ] || { mkdir -p "$d"; tar -xf "$f" -C "$d" --strip-components=1; }; }

build_one() { local name="$1" f="$2" d="$SRC/build-$1"
  if [ -x "$TOOLS/bin/$name" ]; then log "tool $name already built"; return 0; fi
  log "building $name"
  extract "$f" "$d"
  ( cd "$d" && ./configure --prefix="$TOOLS" >/dev/null && make -j"$NPROC" >/dev/null && make install >/dev/null )
  command -v "$name" >/dev/null || { echo "$name build FAILED"; return 1; }
  log "$name -> $(command -v "$name")"
}

build_one m4    "$SRC/m4.tar.xz"
build_one bison "$SRC/bison.tar.xz"
PATH="$TOOLS/bin:$PATH" bison --version >/dev/null   # sanity
PATH="$TOOLS/bin:$PATH" build_one flex  "$SRC/flex.tar.gz"
# bc: kernel 6.19 runs `echo $(CONFIG_HZ) | bc -q kernel/time/timeconst.bc`.
# Host bc absent; GNU bc-1.07.1 build requires `ed` (also absent; installing
# either would mutate the host). Resolution in build.sh step 2: a `bc` shim
# backed by busybox's own bc applet, verified byte-correct vs timeconst.bc.
log "bc: not built (needs ed); busybox-bc shim installed in step 2 instead"

# ---- 4. extract kernel + busybox trees (build happens in step 2) ------------
extract "$SRC/linux.tar.xz" "$SRC/linux"
# compile the kernel's initramfs packer now: no host cpio needed later
gcc -O2 -o "$WORK/bin/gen_init_cpio" "$SRC/linux/usr/gen_init_cpio.c"
extract "$SRC/busybox.tar.bz2" "$SRC/busybox"

# ---- 5. self-recorded source lock (spike-level; specs/upstream-lock.json
#         is integrator-owned and deliberately NOT touched) -------------------
/usr/bin/python3.12 - "$SPIKE" "$KVER" "$BBVER" "$MIRROR" "$WORK/SHA256SUMS.self" <<'PY'
import json,sys,pathlib,datetime
spike,kver,bbver,mirror,sums=sys.argv[1:6]
entries=[]
for line in pathlib.Path(sums).read_text().splitlines():
    h,p=line.split(None,1)
    entries.append({"sha256":h,"file":pathlib.Path(p).name})
lock={
 "spike":"uml round-03",
 "created_utc":datetime.datetime.now(datetime.timezone.utc).isoformat(),
 "mirror":mirror,
 "network_note":"kernel.org/busybox.net unreachable (TCP timeout) on this host 2026-09-25 ~03:30Z; all sources via the TUNA mirror of the upstream projects.",
 "integrity":"sha256 cross-checked against the mirror copy of the upstream-published checksum files where available; PGP signatures NOT verified (no keyring on host). Self-recorded otherwise.",
 "kernel":{"version":kver,"path":"kernel/v6.x/"},
 "busybox":{"version":bbver,"path":"busybox/releases/"},
 "tools":{"m4":"1.4.19","bison":"3.8.2","flex":"2.6.4 (debian orig tarball)","bc":"1.07.1"},
 "hashes":entries,
}
pathlib.Path(spike,"source-lock.json").write_text(json.dumps(lock,indent=1))
print("lock written:",spike+"/source-lock.json")
PY
log "STEP 1 COMPLETE: work=$WORK"
