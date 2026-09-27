#!/bin/busybox sh
# T47 builder/runtime UML init. Mode from kernel cmdline lsbx.mode=build|runtime.
# Evidence channel = /dev/kmsg (phase-a proved /dev/console drops userspace
# writes from this init context). Host OCI cache is exposed via hostfs
# READ-ONLY, dedicated directory only (T47/§13 security boundary).
B=/bin/busybox
L=/lsbx
export PATH=/bin

$B mkdir -p /proc /dev /oci /target
$B mount -t proc proc /proc
$B mount -t sysfs sysfs /sys 2>/dev/null
$B mount -t devtmpfs dev /dev 2>/dev/null
[ -e /dev/kmsg ] || $B mknod /dev/kmsg c 1 11
echo on > /proc/sys/kernel/printk_devkmsg 2>/dev/null || true
exec > /dev/kmsg 2>&1

MODE=unknown
OCIPATH=unknown
for t in $($B cat /proc/cmdline); do
    case "$t" in
        lsbx.mode=*) MODE=${t#lsbx.mode=} ;;
        lsbx.ocipath=*) OCIPATH=${t#lsbx.ocipath=} ;;
    esac
done
echo "LSB_PHASE_INIT mode=$MODE ocipath=$OCIPATH"

# hostfs exposes ONLY the dedicated OCI cache directory, read-only.
$B mount -t hostfs -o "ro,$OCIPATH" hostfs /oci \
    && echo "LSB_HOSTFS_MOUNT_OK" || { echo "LSB_HOSTFS_MOUNT_FAIL"; $B poweroff -f; }
if $B touch /oci/.write-probe 2>/dev/null; then
    echo "LSB_HOSTFS_RO_Violated"
    $B rm -f /oci/.write-probe
else
    echo "LSB_HOSTFS_RO_OK"
fi
$B ls /oci

[ -b /dev/ubda ] || $B mknod /dev/ubda b 98 0 2>/dev/null

probe_path() {
    p="$1"
    if [ ! -e "/target$p" ] && [ ! -L "/target$p" ]; then
        echo "FID|$p|ABSENT"
        return
    fi
    # %F prints multi-word types ("regular file"); put the type LAST and
    # rejoin the tail instead of relying on sed occurrence flags.
    st=$($B stat -c '%u %g %a %i %F' "/target$p" 2>/dev/null)
    set -- $st
    uid=$1; gid=$2; mode=$3; ino=$4
    shift 4
    typ="$*"
    case "$typ" in
        regular*)
            sum=$($B sha256sum < "/target$p" | $B cut -d' ' -f1)
            echo "FID|$p|OK|$uid|$gid|$mode|file|$ino|$sum" ;;
        directory)
            echo "FID|$p|OK|$uid|$gid|$mode|dir|$ino|-" ;;
        symbolic*)
            tgt=$($B readlink "/target$p")
            echo "FID|$p|OK|$uid|$gid|$mode|symlink|$ino|$tgt" ;;
        *)
            echo "FID|$p|OK|$uid|$gid|$mode|$typ|$ino|?" ;;
    esac
}

if [ "$MODE" = build ]; then
    $B mount -t ext4 /dev/ubda /target \
        && echo "LSB_UBD_MOUNT_OK" || { echo "LSB_UBD_MOUNT_FAIL"; $B poweroff -f; }
    echo "LSB_UNPACK_START"
    /tools/umoci-static raw unpack --image /oci/fidelity:latest /target \
        && echo "LSB_UNPACK_OK" || { echo "LSB_UNPACK_FAIL rc=$?"; $B poweroff -f; }
    /tools/umoci-static raw runtime-config --image /oci/fidelity:latest /target/config.json \
        && echo "LSB_RTCONFIG_OK" || echo "LSB_RTCONFIG_FAIL rc=$?"
    echo "LSB_CONFIGSHA=$($B sha256sum /target/config.json | $B cut -d' ' -f1)"
    # ---- in-guest reconciliation probes (guest-side stat is the authority) ----
    for p in $($B cat /oci/probe-paths.txt); do
        probe_path "$p"
    done
    $B sync
    $B umount /target && echo "LSB_UBD_UMOUNT_OK" || echo "LSB_UBD_UMOUNT_FAIL"
    echo "LSB_PHASE_BUILD_DONE"
    $B sleep 1
    $B poweroff -f
fi

if [ "$MODE" = cow ]; then
    # T47-C02 legs. lsbx.leg=A|B|A2|C cmdline selects the step; probes are
    # reported as COW|<path>|<exists>|<sha256-or-'> so the host scores
    # isolation/persistence without trusting guest text beyond raw facts.
    LEG=unknown
    for t in $($B cat /proc/cmdline); do
        case "$t" in lsbx.leg=*) LEG=${t#lsbx.leg=} ;; esac
    done
    $B mount -t ext4 -o rw /dev/ubda /target \
        && echo "LSB_COW_MOUNT_OK" || { echo "LSB_COW_MOUNT_FAIL"; $B poweroff -f; }
    cow_probe() {
        if [ -e "/target$1" ] || [ -L "/target$1" ]; then
            s=$($B sha256sum "/target$1" 2>/dev/null | $B cut -d' ' -f1)
            echo "COW|$1|present|${s:-na}"
        else
            echo "COW|$1|absent|."
        fi
    }
    case "$LEG" in
        A)
            $B mkdir -p /target/cow
            echo marker-a > /target/cow/a-marker
            echo "+A" >> /target/etc/motd.conf
            $B rm -f /target/opt/svc/bin/flag
            $B dd if=/dev/zero of=/target/cow/big1 bs=1M count=1 2>/dev/null
            $B dd if=/dev/zero of=/target/cow/big100 bs=1M count=100 2>/dev/null
            echo "LSB_COW_WRITES_A_OK" ;;
        B)
            # top-level name: /cow exists only in A's own COW layer, not base
            echo marker-b > /target/cow-b-marker
            echo "LSB_COW_WRITES_B_OK" ;;
        C)
            echo marker-c > /target/cow-c-marker
            echo "LSB_COW_WRITES_C_OK" ;;
        A2)
            : ;;  # pure observation leg
    esac
    # uniform probe set across legs (each path written by exactly one leg)
    for p in /cow/a-marker /cow/big1 /cow/big100 /cow-b-marker \
             /cow-c-marker /etc/motd.conf /opt/svc/bin/flag /etc/passwd; do
        cow_probe "$p"
    done
    $B sync
    $B umount /target && echo "LSB_COW_UMOUNT_OK" || echo "LSB_COW_UMOUNT_FAIL"
    echo "LSB_PHASE_COW_${LEG}_DONE"
    $B sleep 1
    $B poweroff -f
fi

if [ "$MODE" = runtime ]; then
    # boot-the-base-disk phase: mount materialized disk, derive the process
    # from the ON-DISK config.json, run it under the configured uid.
    $B mount -t ext4 -o rw /dev/ubda /target \
        && echo "LSB_RT_MOUNT_OK" || { echo "LSB_RT_MOUNT_FAIL"; $B poweroff -f; }
    cfg=/target/config.json
    echo "LSB_RT_CONFIGSHA=$($B sha256sum $cfg | $B cut -d' ' -f1)"
    # umoci raw runtime-config writes PRETTY (multiline, tab-indented) JSON.
    # awk extraction with fixed shapes for the fields we consume.
    CWD=$($B awk -F'"' '/"cwd":/{print $4; exit}' $cfg)
    UIDN=$($B awk '/"uid":/{gsub(/[^0-9]/,""); print; exit}' $cfg)
    GIDN=$($B awk '/"gid":/{gsub(/[^0-9]/,""); print; exit}' $cfg)
    ARGS=$($B awk -F'"' '/"args": \[/{f=1; next} f&&/\]/{f=0} f&&NF{print $2}' $cfg)
    ARGS=$(echo $ARGS)
    ENVS=$($B awk -F'"' '/"env": \[/{f=1; next} f&&/\]/{f=0} f&&NF{print $2}' $cfg)
    ENVS=$(echo $ENVS)
    echo "RT_CFG|cwd=$CWD|uid=$UIDN|gid=$GIDN|args=[$ARGS]|env=[$ENVS]"
    ENVSET=""
    for kv in $ENVS; do
        ENVSET="$ENVSET $kv"
    done
    # resolve the configured uid to a name via the image's own passwd
    # (su needs a name; the fixture's /etc/passwd is part of the image).
    UNAME=$($B awk -F: -v u="$UIDN" '$3==u{print $1; exit}' /target/etc/passwd)
    [ -n "$UNAME" ] || { echo "LSB_RT_USER_UNRESOLVED uid=$UIDN"; $B poweroff -f; }
    # cd into the new root BEFORE chroot: chroot(2) does not move the cwd,
    # and a cwd outside the new root leaves getcwd() unreachable.
    cd /target
    ( export $ENVSET; $B chroot /target /bin/su "$UNAME" -c "echo RT_SU_PWD=\$(pwd); cd $CWD && echo RT_CD_OK || echo RT_CD_FAIL rc=\$?; exec $ARGS" ) \
        || echo "LSB_RT_EXEC_FAIL rc=$?"
    echo "LSB_PHASE_RUNTIME_DONE"
    $B sleep 1
    $B poweroff -f
fi

if [ "$MODE" = control ]; then
    # T47 phase-d guest-control transport. Channel = serial ttyS0 bound to a
    # host fd (ssl0=fd:N) — NEVER the console (Round-03: console opens but
    # silently drops userspace writes on this host).
    # Frame v1 (documented):
    #   REQ  = "LSBXQ" + 8 lowercase hex chars (payload length) + payload
    #   RESP = "LSBXR" + 8 hex chars + payload ("rc=N\n" + raw output bytes)
    #   payload of REQ: line 1 = cwd, following lines = one argv element per
    #   line. Design restriction (recorded): argv elements must not contain
    #   '\n' or NUL — shell line framing; binary-safe RESP payload via file
    #   handoff (command output goes to a file, never through $(...)).
    TTY=/dev/ttyS0
    [ -e "$TTY" ] || $B mknod "$TTY" c 4 64 2>/dev/null
    $B mkdir -p /tmp
    if $B stty raw -echo < "$TTY" 2>/dev/null; then
        echo "LSB_CTL_TTYMODE=raw"
    else
        $B stty -icanon -isig -iexten -ixon -ixoff -icrnl -inlcr -igncr \
              -opost -echo -echoe -echok -echoctl -echoke < "$TTY" 2>/dev/null \
            && echo "LSB_CTL_TTYMODE=fallback" || echo "LSB_CTL_TTYMODE=fail"
    fi
    exec 3<>"$TTY" 4>"$TTY"
    echo "LSB_CTL_READY"
    while true; do
        hdr=$($B dd bs=1 count=13 <&3 2>/dev/null)
        # empty read = channel disturbance (or peer gone): back off, do NOT
        # spin-log (observed: BADHDR storm after a bad-control verb)
        [ -z "$hdr" ] && { $B sleep 1; continue; }
        case "$hdr" in
            LSBXQ????????) len=$((0x${hdr#LSBXQ})) ;;
            *) echo "LSB_CTL_BADHDR" ; continue ;;
        esac
        $B dd bs=1 count=$len <&3 > /tmp/ctlreq 2>/dev/null
        workdir=$($B sed -n '1p' /tmp/ctlreq)
        set --
        $B sed -n '2,$p' /tmp/ctlreq > /tmp/ctlargv
        while IFS= read -r a; do set -- "$@" "$a"; done < /tmp/ctlargv
        ( cd "$workdir" 2>/dev/null && "$@" ) > /tmp/ctlout 2>&1
        rc=$?
        { printf 'rc=%s\n' "$rc"; $B cat /tmp/ctlout; } > /tmp/ctlresp
        { $B printf 'LSBXR%08x' $($B wc -c < /tmp/ctlresp); $B cat /tmp/ctlresp; } >&4
    done
fi

echo "LSB_PHASE_UNKNOWN_MODE"
$B sleep 1
$B poweroff -f
