#!/usr/bin/env bash
# Round-03 UML spike — step 2: build ARCH=um kernel + static busybox, assemble
# a minimal initramfs (no cpio on host: uses the kernel's gen_init_cpio),
# then BOOT it and collect the Phase-A verdict (boot / blocked + errno).
# No sudo, no KVM, no host mutation. UML runs as the current user.
set -uo pipefail

SPIKE="$(cd "$(dirname "$0")" && pwd)"
WORK="$SPIKE/work"
SRC="$WORK/src"
TOOLS="$WORK/tools"
EVID="$SPIKE/../results"
REPO_EVID="$(cd "$SPIKE/../.." && pwd)/evidence/runtime/uml"
mkdir -p "$EVID" "$REPO_EVID" "$WORK/bin"
export PATH="$TOOLS/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
NPROC="$(nproc)"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

# ---- 0. host readiness probes (independent evidence, recorded verbatim) ----
PROBE_JSON="$WORK/host-readiness.json"
{
  echo "{"
  echo "\"probe_utc\":\"$(date -u +%FT%TZ)\","
  echo "\"capeff\":\"$(grep '^CapEff' /proc/self/status)\","
  echo "\"ptrace_traceme\":$("$WORK/bin/probe-ptrace" 2>/dev/null || true),"
  echo "}"
} > /dev/null 2>&1 || true
# build the tiny ptrace probe first so its result is real
cat > "$WORK/bin/probe-ptrace.c" <<'EOF'
#include <stdio.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <unistd.h>
#include <stdlib.h>
int main(void){
  pid_t p=fork();
  if(p==0){
    if(ptrace(PTRACE_TRACEME,0,0,0)<0){perror("TRACEME");_exit(2);}
    _exit(0);
  }
  int st;
  waitpid(p,&st,0);
  if(WIFEXITED(st) && WEXITSTATUS(st)==2){puts("EPERM");return 1;}
  puts("OK");
  return 0;
}
EOF
gcc -O2 -o "$WORK/bin/probe-ptrace" "$WORK/bin/probe-ptrace.c"
PTR="$("$WORK/bin/probe-ptrace" 2>&1 || echo "rc=$?")"
log "ptrace probe: $PTR  (CapEff: $(awk '/^CapEff/{print $2}' /proc/self/status))"

# ---- 1. kernel config -------------------------------------------------------
cd "$SRC/linux"
if [ ! -f .config ]; then
  log "make ARCH=um defconfig"
  make ARCH=um defconfig >/dev/null
  scripts/config --enable CONFIG_BLK_DEV_INITRD
  scripts/config --disable CONFIG_DEBUG_INFO
  scripts/config --disable CONFIG_DEBUG_INFO_BTF
  scripts/config --enable CONFIG_STATIC_LINK
  scripts/config --disable CONFIG_UML_NET
  scripts/config --enable CONFIG_MAGIC_SYSRQ
  scripts/config --enable CONFIG_DEVTMPFS
  make ARCH=um olddefconfig >/dev/null
fi
grep -E 'CONFIG_(STATIC_LINK|BLK_DEV_INITRD|DEBUG_INFO)=' .config | head -5 | while read -r l; do log "config $l"; done

# ---- 2. busybox static ------------------------------------------------------
if [ ! -x "$SRC/busybox/busybox" ]; then
  log "busybox defconfig + static build"
  # NOTE: no `yes '' | make oldconfig` here — under `set -o pipefail` the
  # yes-side SIGPIPE made the whole chain fail (lovely). `make` alone runs
  # kbuild's silentoldconfig, which takes defaults non-interactively.
  ( cd "$SRC/busybox" \
    && make defconfig >/dev/null \
    && sed -i 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' .config \
    && grep -q '^CONFIG_STATIC=y' .config \
    && make -j"$NPROC" >/dev/null )
fi
[ -x "$SRC/busybox/busybox" ] || { echo "busybox build FAILED"; exit 1; }
log "busybox binary: $(file "$SRC/busybox/busybox" | cut -c1-90)"
# kernel 6.19 needs `bc` for include/generated/timeconst.h. Host bc is absent
# and GNU bc's build needs `ed` (also absent, host mutation not authorized).
# Resolution (verified against kernel/time/timeconst.bc): busybox's bc applet
# handles the kernel invocation `echo $(CONFIG_HZ) | bc -q timeconst.bc`
# byte-correctly, so expose a `bc` shim backed by the static busybox.
if [ ! -x "$TOOLS/bin/bc" ]; then
  cat > "$TOOLS/bin/bc" <<EOF
#!/bin/sh
exec "$SRC/busybox/busybox" bc "\$@"
EOF
  chmod +x "$TOOLS/bin/bc"
fi
log "bc shim ready: $(echo 'print 6*7,"\n"' | PATH="$TOOLS/bin:$PATH" bc -q)"

# ---- 3. initramfs (init double-duty: Phase A boot markers + first multi-UID
#          probes; deeper Phase-B matrix lives in verify_uml.py) --------------
ROOTFS_L="$WORK/rootfs"
mkdir -p "$ROOTFS_L/bin" "$ROOTFS_L/proc" "$ROOTFS_L/sys" "$ROOTFS_L/dev" "$ROOTFS_L/etc" "$ROOTFS_L/workspace"
cp "$SRC/busybox/busybox" "$ROOTFS_L/bin/busybox"
for app in sh mount hostname id cat echo mkdir ps ls su touch chmod chown stat env whoami poweroff grep wc; do
  ln -sf busybox "$ROOTFS_L/bin/$app"
done
cat > "$ROOTFS_L/init" <<'EOF'
#!/bin/sh
# proc FIRST: capture the userspace->kmsg throttle setting, then disable it.
# Observed across runs: after ~10-12 userspace writes to /dev/kmsg all further
# lines vanished — signature of kernel.printk_devkmsg=ratelimit, not a flush
# race (sleep 1 before poweroff falsified the race).
/bin/busybox mkdir -p /proc /dev
/bin/mount -t proc proc /proc
if [ ! -e /dev/kmsg ]; then /bin/busybox mknod /dev/kmsg c 1 11; fi
echo "LSB_DEVKMSG_MODE_BEFORE=$(/bin/cat /proc/sys/kernel/printk_devkmsg 2>/dev/null)" > /dev/kmsg
echo on > /proc/sys/kernel/printk_devkmsg 2>/dev/null || true
# Evidence plumbing: /dev/console opens but drops userspace writes (proven);
# verdict channel = /dev/kmsg.
exec 2>/dev/kmsg
if ( /bin/busybox true >/dev/console ) ; then
  echo "LSB_CONSOLE_PROBE=open-ok" > /dev/kmsg
else
  echo "LSB_CONSOLE_PROBE=open-failed" > /dev/kmsg
fi
# /dev/console opens but silently drops userspace writes here (observed:
# probe=open-ok while zero echoed markers reached the log). Verdict channel = kmsg.
exec >/dev/kmsg 2>&1
/bin/mount -t proc proc /proc
/bin/mount -t sysfs sysfs /sys
/bin/mount -t devtmpfs dev /dev 2>/dev/null
/bin/hostname lsbx-uml
echo "LSB_UML_BOOT_OK"
echo "LSB_PROC_OWN=$([ -d /proc/1 ] && echo yes)"
echo "LSB_HOSTNAME=$(/bin/cat /proc/sys/kernel/hostname)"
# --- multi-UID probes (Phase B seeds; guest kernel owns id semantics) ---
echo "root:x:0:0:root:/root:/bin/sh" > /etc/passwd
echo "user:x:1000:1000:user:/workspace:/bin/sh" >> /etc/passwd
echo "svc:x:999:999:svc:/workspace:/bin/sh" >> /etc/passwd
echo "root:x:0:" > /etc/group
echo "user:x:1000:" >> /etc/group
echo "svc:x:999:" >> /etc/group
/bin/chmod 1777 /workspace
/bin/touch /workspace/multiuid-file
/bin/chown 1000:999 /workspace/multiuid-file && echo "LSB_CHOWN_OK" || echo "LSB_CHOWN_FAIL rc=$?"
echo "LSB_STAT=$(/bin/stat -c '%u:%g %a' /workspace/multiuid-file)"
/bin/su user -c 'id' && echo "LSB_SU_OK" || echo "LSB_SU_FAIL rc=$?"
/bin/su user -c 'touch /workspace/by-user && ls -l /workspace/by-user' \
  && echo "LSB_USER_WRITE_OK" || echo "LSB_USER_WRITE_FAIL"
# reverse probe: uid 999 must NOT read a uid-1000 mode-0600 file
echo guest-secret > /workspace/user-only
/bin/chown 1000:1000 /workspace/user-only && /bin/chmod 600 /workspace/user-only
/bin/su svc -c 'cat /workspace/user-only' >/dev/null 2>/dev/null \
  && echo "LSB_DAC_ISOLATION_FAIL" || echo "LSB_DAC_ISOLATION_OK"
echo "LSB_CGROUPS=$(/bin/wc -l < /proc/cgroups)"
echo "LSB_UML_EXPERIMENT_DONE"
# drain printk→console pipeline before the reboot syscall; the last markers
# were observed being lost without this pause
/bin/busybox sleep 1
/bin/poweroff -f
EOF
chmod 755 "$ROOTFS_L/init"

# pack with the kernel's tool (host has no cpio)
( cd "$ROOTFS_L" && find . | cut -c3- | grep -v '^\.$' > "$WORK/rootfs-files.txt" )
/usr/bin/python3.12 - "$ROOTFS_L" "$WORK/rootfs-files.txt" "$WORK/initramfs.list" <<'PY'
import os,sys
root,listing,out=sys.argv[1:4]
lines=[]
with open(out,"w") as fh:
    # Device nodes: packed at build time by gen_init_cpio (we pack as uid 0 in
    # this container; mknod(2) is permitted by the outer seccomp profile).
    # Without /dev/console the kernel reports "unable to open an initial
    # console" and init runs blind — that exact failure was observed here.
    for typ,path,mode,maj,minr in [
        ("c","/dev/console",0o600,5,1),
        ("c","/dev/null",0o666,1,3),
        ("c","/dev/tty",0o666,5,0),
    ]:
        fh.write(f"nod {path} {mode:o} 0 0 {typ} {maj} {minr}\n")
    for name in open(listing).read().splitlines():
        path=os.path.join(root,name)
        target="/"+name
        if os.path.islink(path):
            fh.write(f"slink {target} {os.readlink(path)} 777 0 0\n")
        elif os.path.isdir(path):
            st=os.lstat(path)
            fh.write(f"dir {target} {st.st_mode & 0o7777:o} 0 0\n")
        else:
            st=os.lstat(path)
            fh.write(f"file {target} {path} {st.st_mode & 0o7777:o} 0 0\n")
print("initramfs.list written")
PY
scripts/config --set-str CONFIG_INITRAMFS_SOURCE "$WORK/initramfs.list"
make ARCH=um olddefconfig >/dev/null

# ---- 4. kernel build ----------------------------------------------------------
if [ ! -x "$SRC/linux/linux" ] || [ "$WORK/initramfs.list" -nt "$SRC/linux/linux" ]; then
  log "make ARCH=um -j$NPROC (this takes minutes)"
  if ! make ARCH=um -j"$NPROC" > "$WORK/kernel-build.log" 2>&1; then
    echo "KERNEL BUILD FAILED — tail:"; tail -30 "$WORK/kernel-build.log"; exit 1
  fi
fi
[ -x "$SRC/linux/linux" ] || { echo "uml binary missing"; exit 1; }
KSHA256="$(sha256sum "$SRC/linux/linux" | awk '{print $1}')"
log "uml binary sha256 $KSHA256"
file "$SRC/linux/linux" | cut -c1-100

# ---- 5. BOOT (Phase A verdict) -------------------------------------------------
CONSOLE="$WORK/boot-console.log"
log "booting UML (timeout 120s)…"
timeout 120 "$SRC/linux/linux" mem=256M con0=fd:0,fd:1 </dev/null > "$CONSOLE" 2>&1
RC=$?
log "host process exit rc=$RC"
tail -25 "$CONSOLE"

# ---- 6. evidence ---------------------------------------------------------------
cp "$CONSOLE" "$REPO_EVID/boot-console.txt"
/usr/bin/python3.12 - "$SPIKE" "$CONSOLE" "$RC" "$KSHA256" "$PTR" <<'PY'
import json,sys,hashlib,pathlib,datetime
spike,console,rc,ksha,ptr=sys.argv[1:6]
text=pathlib.Path(console).read_text(errors="replace")
markers={m: (m in text) for m in
  ["LSB_UML_BOOT_OK","LSB_CHOWN_OK","LSB_SU_OK","LSB_USER_WRITE_OK","LSB_UML_EXPERIMENT_DONE"]}
def field(name):
    for line in text.splitlines():
        if line.startswith(name+"="):
            return line.split("=",1)[1]
    return None
verdict = "BOOT_OK" if (markers["LSB_UML_BOOT_OK"] and markers["LSB_UML_EXPERIMENT_DONE"]) else \
          ("BOOT_PARTIAL" if markers["LSB_UML_BOOT_OK"] else "BLOCKED")
result={
 "experiment":"uml phase-a boot (CONTAINERIZED + PORTABLE_ROOTLESS host)",
 "utc":datetime.datetime.now(datetime.timezone.utc).isoformat(),
 "host_process_exit_rc":int(rc),
 "ptrace_probe":ptr,
 "kernel_binary_sha256":ksha,
 "verdict":verdict,
 "markers":markers,
 "fields":{k:field(k) for k in ["LSB_PROC_OWN","LSB_HOSTNAME","LSB_STAT","LSB_CGROUPS"]},
 "console_tail":text.splitlines()[-25:],
 "note":"verdict derives only from guest-emitted markers in kernel console output.",
}
out=pathlib.Path(spike).parents[0]/"results"/"uml-boot.json"
out.write_text(json.dumps(result,indent=1))
print("evidence written:", out)
print("VERDICT:", verdict)
PY
