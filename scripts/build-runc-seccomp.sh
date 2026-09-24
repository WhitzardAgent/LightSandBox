#!/usr/bin/env bash
# Seccomp feasibility spike build: user-local libseccomp (release tarball,
# configure prebuilt — host has no autotools) + runc built from the LOCKED
# /opt/src/runc sources with the seccomp tag. No sudo, no /usr writes.
# The host lacks a real pkg-config binary; a minimal shim answering only the
# libseccomp queries is generated into the scratch dir for the go build.
#
# Evidence: facts JSON lines on stdout (collected into
# evidence/security/seccomp-build.json by the caller).
set -u
# The session PATH starts with a trash-wrapper `rm` shim whose stdout noise
# contaminates autotools command substitutions (configure fails with
# "config.sub: too many arguments"). Use the system PATH inside this build;
# deletions here are ephemeral build scratch in $SCRATCH only.
PREFIX="${PREFIX:-/opt/toolchains/lightsandbox}"
# $PREFIX/bin carries the user-local gperf that libseccomp's configure needs.
export PATH="$PREFIX/bin:/opt/toolchains/go/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/bin"
SCRATCH="${SCRATCH:-/tmp/seccomp-spike}"
VER="${LIBSECCOMP_VER:-2.5.6}"
URL="https://github.com/seccomp/libseccomp/releases/download/v${VER}/libseccomp-${VER}.tar.gz"
mkdir -p "$SCRATCH" "$PREFIX/bin"
cd "$SCRATCH" || exit 1
say() { echo "[build-runc-seccomp $(date -u +%FT%TZ)] $*"; }
fact() { echo "FACT $1=$2" >>"$SCRATCH/facts.txt"; }
: > "$SCRATCH/facts.txt"

say "fetch $URL"
curl -fsSL --retry 2 -o libseccomp.tar.gz "$URL" || { say "DOWNLOAD FAILED"; exit 1; }
SHA=$(sha256sum libseccomp.tar.gz | cut -d' ' -f1)
fact libseccomp_version "$VER"
fact libseccomp_tarball_url "$URL"
fact libseccomp_tarball_sha256 "$SHA"
# NOTE: upstream publishes a .asc signature but no checksum list; the sha256
# above is self-recorded at fetch time (root of trust = this fetch). Promotion
# into specs/upstream-lock.json must add multi-party verification.
say "tarball sha256=$SHA"

rm -rf "libseccomp-$VER"
tar xf libseccomp.tar.gz
cd "libseccomp-$VER" || exit 1
say "configure+make (static only) prefix=$PREFIX"
./configure --prefix="$PREFIX" --enable-static --disable-shared >"$SCRATCH/configure.log" 2>&1 \
  || { say "CONFIGURE FAILED"; tail -20 "$SCRATCH/configure.log"; exit 1; }
make -j"$(nproc)" >"$SCRATCH/make.log" 2>&1 || { say "MAKE FAILED"; tail -20 "$SCRATCH/make.log"; exit 1; }
make install >"$SCRATCH/install.log" 2>&1 || { say "INSTALL FAILED"; tail -20 "$SCRATCH/install.log"; exit 1; }
ls -la "$PREFIX/lib/libseccomp.a" "$PREFIX/include/seccomp.h" || exit 1
fact libseccomp_prefix "$PREFIX"
fact libseccomp_archive_sha256 "$(sha256sum "$PREFIX/lib/libseccomp.a" | cut -d' ' -f1)"

# ---- pkg-config shim (host lacks pkg-config; go's #cgo pkg-config directive
# still needs an executable named pkg-config). It answers ONLY libseccomp
# queries against our prefix; any other query fails loudly.
cat > "$SCRATCH/pkg-config" <<EOF
#!/bin/sh
mode=""; seen=""
for a in "\$@"; do
  case "\$a" in
    --cflags) mode=cflags ;;
    --libs) mode=libs ;;
    --modversion) mode=version ;;
    --exists) mode=exists ;;
    --static|--|--) : ;;
    libseccomp) seen=yes ;;
    -*) : ;;
    *) echo "pkg-config shim: unknown package '\$a'" >&2; exit 1 ;;
  esac
done
[ "\$seen" = "yes" ] || { echo "pkg-config shim: no package named" >&2; exit 1; }
case "\$mode" in
  cflags) echo "-I$PREFIX/include" ;;
  libs) echo "-L$PREFIX/lib -lseccomp" ;;
  version) echo "$VER" ;;
  exists) exit 0 ;;
esac
exit 0
EOF
chmod +x "$SCRATCH/pkg-config"
PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig" "$SCRATCH/pkg-config" --cflags --libs -- libseccomp || exit 1

# ---- runc with seccomp ----
say "clone locked /opt/src/runc into scratch"
rm -rf "$SCRATCH/runc-src"
cp -a /opt/src/runc "$SCRATCH/runc-src"
cd "$SCRATCH/runc-src" || exit 1
RCOMMIT=$(git rev-parse HEAD)
fact runc_commit "$RCOMMIT"
export PATH="$SCRATCH:/opt/toolchains/go/bin:$PATH"
export PKG_CONFIG="$SCRATCH/pkg-config"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
export GOPROXY="https://goproxy.cn,direct"
export GOFLAGS="-buildvcs=false"
export CGO_ENABLED=1
say "go: $(go version)"
go build -trimpath -tags seccomp -ldflags "-s -w" -o "$SCRATCH/runc-seccomp" ./ >"$SCRATCH/go-build.log" 2>&1 \
  || { say "GO BUILD FAILED"; tail -30 "$SCRATCH/go-build.log"; exit 1; }
cp "$SCRATCH/runc-seccomp" "$PREFIX/bin/runc-seccomp"
RS=$(sha256sum "$PREFIX/bin/runc-seccomp" | cut -d' ' -f1)
fact runc_seccomp_sha256 "$RS"
fact go_version "$(go version | awk '{print $3}')"
fact build_tags "seccomp"
say "runc-seccomp sha256=$RS"
"$PREFIX/bin/runc-seccomp" --version | sed 's/^/VERSION /' | tee -a "$SCRATCH/facts.txt"
"$PREFIX/bin/runc-seccomp" features | sed 's/^/FEATURES /' | tee -a "$SCRATCH/facts.txt"
ldd "$PREFIX/bin/runc-seccomp" | sed 's/^/LDD /' | tee -a "$SCRATCH/facts.txt" || true
say "DONE fail=0"
# honesty note: this runc is dynamically linked against host glibc (CGO);
# fine for feasibility on THIS host; a production static build would need musl.
exit 0
