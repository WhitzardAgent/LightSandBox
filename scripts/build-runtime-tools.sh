#!/usr/bin/env bash
# Build runc + umoci from the locked /opt/src sources into a user-level
# directory. No sudo, no /usr writes. Records compiler/commit/sha256 evidence.
# Usage: scripts/build-runtime-tools.sh [DEST]  (default: ~/.local/libexec/lightsandbox)
set -u
DEST="${1:-$HOME/.local/libexec/lightsandbox}"
BIN="$DEST/bin"
EVID="$DEST/evidence"
mkdir -p "$BIN" "$EVID"
export PATH="/opt/toolchains/go/bin:$PATH"
export GOPROXY="https://goproxy.cn,direct"
export GOFLAGS="-buildvcs=false"
export CGO_ENABLED=0

LOG="$EVID/build-log.txt"
: > "$LOG"
say() { echo "[$(date -u +%FT%TZ)] $*" | tee -a "$LOG"; }

say "go: $(go version)"
mkdir -p "$DEST/src"
copy_locked() {  # $1=name -> copies /opt/src/$1 to $DEST/src/$1 (fresh)
  cp -a "/opt/src/$1" "$DEST/src/$1.tmp" && rm -rf "$DEST/src/$1.old" 2>/dev/null
  rm -rf "$DEST/src/$1" && mv "$DEST/src/$1.tmp" "$DEST/src/$1"
}

fail=0

# ---- runc (seccomp disabled: no libseccomp headers on this host) ----
say "== runc =="
copy_locked runc
cd "$DEST/src/runc" || exit 1
RCOMMIT=$(git rev-parse HEAD 2>/dev/null || echo unknown)
if make static BUILDTAGS="" EXTRA_FLAGS="" >>"$LOG" 2>&1 && [ -x runc ]; then
  cp runc "$BIN/runc"
  say "runc built: commit=$RCOMMIT sha256=$(sha256sum "$BIN/runc" | cut -d' ' -f1)"
  "$BIN/runc" --version | tee -a "$LOG"
else
  say "runc build FAILED (see $LOG)"; fail=1
fi

# ---- umoci ----
say "== umoci =="
copy_locked umoci
cd "$DEST/src/umoci" || exit 1
UCOMMIT=$(git rev-parse HEAD 2>/dev/null || echo unknown)
if go build -o "$BIN/umoci" ./cmd/umoci >>"$LOG" 2>&1 && [ -x "$BIN/umoci" ]; then
  say "umoci built: commit=$UCOMMIT sha256=$(sha256sum "$BIN/umoci" | cut -d' ' -f1)"
  "$BIN/umoci" --version | tee -a "$LOG" || true
else
  say "umoci build FAILED (see $LOG)"; fail=1
fi

say "done fail=$fail"
exit "$fail"
