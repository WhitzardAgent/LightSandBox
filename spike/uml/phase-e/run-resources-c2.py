#!/usr/bin/env python3.12
"""UML resource-semantics spike on kernel-candidate-2 (round-05 §8).

Additive successor of phase-e/run-resources.py (candidate-1, which stays
pinned history in results/uml-resource-semantics.json). Candidate-2 flips
CONFIG_MEMCG=y, CONFIG_CGROUP_PIDS=y, CONFIG_UML_NET_VECTOR=y and unsets
CONFIG_MODULES, so EVERYTHING candidate-1 could only record (MEMCG offered,
R3 NOT_TESTED) is a REAL assertion here. Check intent, unchanged from v2:

  R1  mem=256M sizes the GUEST only (MemTotal within (200MiB, 256MiB]).
  R2  ~150MiB tmpfs write costs the HOST process real RSS (VmHVM/VmHWM >=
      140MiB) — guest memory is host memory, observed from host /proc.
  R3  guest-internal hard limit: cgroup 'memory' MUST be offered
      (CONFIG_MEMCG=y build fact) and memory.max=32MiB MUST be enforced
      (allocator SIGKILLed) with init+channel surviving. No NOT_TESTED
      branch outcome is acceptable on candidate-2 — a missing controller is
      a MISMATCH against the build facts, never re-recorded as truth.
  R4a anon-memory hog (~300MiB) is OOM-killed; init, channel and host
      process all survive (fault containment for anonymous memory).
  R4b shmem oversubscription without swap is guest-FATAL by kernel
      design (kill frees nothing); the death itself is the asserted
      expected fact — never claimed otherwise.
  R5  nproc truth recorded verbatim.

Candidate-2 additions (round-05 §8):
  * kernel binary/config sha256 + running kernelrelease captured at runtime.
  * 'pids' offered is a REAL assertion (CONFIG_CGROUP_PIDS=y); pids.max=16
    must keep a 64-fork storm at <= 20 tasks in the cgroup (slack for the
    shell + count race; observed value recorded verbatim).
  * 'cpu' offered is a REAL assertion; cpu.max=50000/100000 (50%) must
    hold a 3s CPU hog to <= 2400000 usec delta (loose 80% upper bound
    against shared-host noise; measured value recorded verbatim).
  * module-policy probe: MODULES=n must make /proc/modules absent AND
    make busybox insmod fail (rc!=0). Both outputs recorded verbatim.
    GuestWorkloadHardLimit (memory.max/pids.max/cpu.max inside the GUEST
    cgroup v2 hierarchy — what this run asserts) vs
    HostSandboxProcessHardLimit (limits on the host UML process itself —
    UNSUPPORTED on this host, cgroupfs ro, same boundary as T09-C01).
"""
import datetime
import hashlib
import importlib.util
import json
import pathlib
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "rc", HERE.parent / "phase-d" / "run-control.py")
rc_mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rc_mod)

PD = rc_mod.WORK / "phase-e"
PD.mkdir(parents=True, exist_ok=True)
RESULTS = HERE.parent.parent / "results"
KERNEL_BIN = pathlib.Path(rc_mod.KERNEL)
KERNEL_CFG = rc_mod.WORK / "src/linux/.config"

checks = []
notes = []
FACTS = {}  # run() fills; finalize() reads (kept scope-explicit)


def chk(name, cond, want_desc, obs):
    checks.append({"check": name, "expected": want_desc, "observed": obs,
                   "verdict": "MATCH" if cond else "MISMATCH"})


def _sha256(p):
    return hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()


def sh(g, script, timeout=90, tag=""):
    print(f"  [leg {tag}] {script[:60]!r}", flush=True)
    r = g.exec(["/bin/sh", "-c", script], cwd="/", timeout=timeout)
    print(f"  [leg {tag}] done rc={r[0]} out={len(r[1])}B", flush=True)
    return r


def out_text(out):
    return out.decode(errors="replace")


def marker(txt, key, default=None):
    for tok in txt.replace("\n", " ").split():
        if tok.startswith(key + "="):
            val = tok.split("=", 1)[1]
            try:
                return int(val)
            except ValueError:
                return val
    return default


def run():
    # captured first, so even a boot crash still writes build facts
    FACTS["kernel_binary_sha256"] = _sha256(KERNEL_BIN)
    FACTS["kernel_config_sha256"] = _sha256(KERNEL_CFG)
    FACTS["kernel_binary_path"] = str(KERNEL_BIN)
    FACTS["kernel_config_path"] = str(KERNEL_CFG)
    rc_mod.rebuild_initramfs()
    umid = "lsbxresc2"
    logp = PD / "console-resources-c2.log"
    g = rc_mod.Guest(umid, logp)
    print(f"boot umid={umid} pid={g.proc.pid}", flush=True)
    g.wait_ready(logp)

    # kernelrelease truth, from the RUNNING guest — never hand-copied
    rc, out = sh(g, "uname -r", tag="KREL")
    FACTS["guest_kernelrelease"] = out_text(out).strip()

    # R1
    rc, out = sh(g, "awk '/MemTotal/{print $2}' /proc/meminfo", tag="R1")
    mem_kib = int(out.strip())
    FACTS["guest_mem_total_kib"] = mem_kib
    chk("R1 mem=256M guest MemTotal KiB in (204800, 262144]",
        204800 < mem_kib <= 262144, "(204800, 262144]", mem_kib)

    # R5 (recorded)
    rc, out = sh(g, "grep -c ^processor /proc/cpuinfo; "
                    "cat /sys/devices/system/cpu/online 2>/dev/null", tag="R5")
    cpu_fields = out_text(out).split()
    FACTS["guest_cpu_fields"] = cpu_fields
    chk("R5 nproc recorded", len(cpu_fields) >= 1, ">=1 field", cpu_fields)

    # cgroup v2 + controller inventory (THE decisive read before R3)
    rc, out = sh(g, "mount -t cgroup2 none /sys/fs/cgroup 2>&1; "
                    "echo mount_rc=$?; "
                    "cat /sys/fs/cgroup/cgroup.controllers 2>/dev/null",
                 tag="CG")
    txt = out_text(out)
    cg_ok = "mount_rc=0" in txt
    chk("cgroup2 mounts in guest", cg_ok, "mount_rc=0", txt.strip()[:120])
    controllers = ""
    if cg_ok:
        tail = txt.splitlines()
        controllers = tail[-1] if tail and "mount_rc" not in tail[-1] else ""
    ctrl_list = controllers.split()
    has_memcg = "memory" in ctrl_list
    has_pids = "pids" in ctrl_list
    has_cpu = "cpu" in ctrl_list
    FACTS["guest_cgroup_controllers"] = controllers.strip()
    FACTS["memcg_present"] = has_memcg
    FACTS["pids_cg_present"] = has_pids
    FACTS["cpu_cg_present"] = has_cpu
    # candidate-2 build facts make these REAL assertions (c1 recorded only)
    chk("MEMCG offered in guest cgroup.controllers", has_memcg,
        "'memory' offered (CONFIG_MEMCG=y build fact)",
        controllers.strip() or "EMPTY")
    notes.append("candidate-1 finding: CONFIG_MEMCG absent (R3 NOT_TESTED); "
                 "candidate-2 asserts it by build fact"
                 if has_memcg else "MEMCG ABSENT against candidate-2 build")

    # R3 guest-internal hard limit — real on candidate-2 by build fact
    sh(g, "mkdir -p /tmp/shm; mount -t tmpfs -o size=180m tmpfs /tmp/shm")
    if cg_ok and has_memcg:
        script = (
            "echo +memory > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null; "
            "mkdir /sys/fs/cgroup/t; "
            "echo 33554432 > /sys/fs/cgroup/t/memory.max; "
            "echo $$ > /sys/fs/cgroup/t/cgroup.procs; "
            "dd if=/dev/zero of=/tmp/shm/f32 bs=1M count=100 2>/dev/null; "
            "echo inner_rc=$?")
        rc, out = sh(g, script, timeout=90)
        txt = out_text(out)
        inner = [l for l in txt.splitlines() if l.startswith("inner_rc=")]
        inner_rc = int(inner[0].split("=")[1]) if inner else -1
        chk("R3 guest enforces 32MiB memory.max (allocator SIGKILL=137)",
            inner_rc == 137, "inner_rc=137", inner_rc)
        rc2, out2 = sh(g, "echo alive-after-memcg-oom")
        chk("R3 init+channel survive MEMCG-constrained OOM",
            rc2 == 0 and b"alive-after-memcg-oom" in out2, "echo ok",
            out_text(out2).strip()[:60])
        sh(g, "echo $$ > /sys/fs/cgroup/cgroup.procs; "
              "rmdir /sys/fs/cgroup/t 2>/dev/null; rm -f /tmp/shm/f32")
    else:
        checks.append({"check": "R3 guest memory.max enforcement",
                       "expected": "MEMCG present (CONFIG_MEMCG=y build fact)",
                       "observed": "NOT_TESTED — 'memory' not in guest "
                                   "cgroup.controllers on candidate-2",
                       "verdict": "MISMATCH"})  # build fact violated

    # --- candidate-2 additions (round-05 §8): pids / cpu / module policy ---

    chk("pids offered in guest cgroup.controllers", has_pids,
        "'pids' offered (CONFIG_CGROUP_PIDS=y build fact)",
        controllers.strip() or "EMPTY")
    if cg_ok and has_pids:
        # pids.max=16 vs a 64-fork storm of `sleep 30`; shell itself counts
        # toward the limit, so a hard 16 + count-race slack => assert <= 20
        script = (
            "echo +pids > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null; "
            "mkdir /sys/fs/cgroup/t2; "
            "echo 16 > /sys/fs/cgroup/t2/pids.max; "
            "echo $$ > /sys/fs/cgroup/t2/cgroup.procs; "
            "i=0; while [ $i -lt 64 ]; do sleep 30 & i=$((i+1)); done; "
            "sleep 1; "
            "echo PIDSCUR=$(cat /sys/fs/cgroup/t2/pids.current)")
        rc, out = sh(g, script, timeout=60, tag="PIDS")
        pids_cur = marker(out_text(out), "PIDSCUR", -1)
        FACTS["pids_current_with_max16_vs_64forks"] = pids_cur
        chk("guest pids.max=16 caps 64-fork storm at <=20 tasks",
            isinstance(pids_cur, int) and 0 < pids_cur <= 20,
            "1 <= pids.current <= 20 (hard 16 + shell/race slack)",
            pids_cur)
        rc2, out2 = sh(g, "echo alive-after-pids-limit")
        chk("guest channel alive after pids fork storm",
            rc2 == 0 and b"alive-after-pids-limit" in out2, "echo ok",
            out_text(out2).strip()[:60])
        sh(g, "kill $(cat /sys/fs/cgroup/t2/cgroup.procs 2>/dev/null) "
              "2>/dev/null; sleep 1; "
              "for p in $(cat /sys/fs/cgroup/t2/cgroup.procs 2>/dev/null); "
              "do echo $p > /sys/fs/cgroup/cgroup.procs 2>/dev/null; done; "
              "rmdir /sys/fs/cgroup/t2 2>/dev/null")
    else:
        checks.append({"check": "guest pids.max enforcement",
                       "expected": "pids present (CONFIG_CGROUP_PIDS=y)",
                       "observed": "NOT_TESTED — 'pids' not offered on "
                                   "candidate-2", "verdict": "MISMATCH"})

    chk("cpu offered in guest cgroup.controllers", has_cpu,
        "'cpu' offered (build fact)", controllers.strip() or "EMPTY")
    if cg_ok and has_cpu:
        # cpu.max 50000/100000 = 50% of one CPU; a 3s dd hog may consume
        # at most ~1.5s of cgroup CPU. Bound is loose (80%) only against
        # shared-host scheduling noise; the measured delta is the record.
        script = (
            "echo +cpu > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null; "
            "mkdir /sys/fs/cgroup/t3; "
            "echo '50000 100000' > /sys/fs/cgroup/t3/cpu.max; "
            "echo CMAXW=$?; "
            "echo $$ > /sys/fs/cgroup/t3/cgroup.procs; "
            "U1=$(awk '/usage_usec/{print $2}' /sys/fs/cgroup/t3/cpu.stat); "
            "dd if=/dev/zero of=/dev/null 2>/dev/null & "
            "HogP=$!; sleep 3; kill $HogP 2>/dev/null; wait 2>/dev/null; "
            "U2=$(awk '/usage_usec/{print $2}' /sys/fs/cgroup/t3/cpu.stat); "
            "echo CPUBEFORE=$U1 CPUAFTER=$U2")
        rc, out = sh(g, script, timeout=60, tag="CPU")
        txt = out_text(out)
        cmaxw = marker(txt, "CMAXW", -1)
        ub = marker(txt, "CPUBEFORE", -1)
        ua = marker(txt, "CPUAFTER", -1)
        delta = ua - ub if isinstance(ua, int) and isinstance(ub, int) else -1
        FACTS["cpu_stat_usage_usec_before"] = ub
        FACTS["cpu_stat_usage_usec_after"] = ua
        FACTS["cpu_stat_delta_usec_3s_hog_50pct_cap"] = delta
        chk("cpu.max write rc=0 ('50000 100000')", cmaxw == 0, 0, cmaxw)
        chk("guest cpu.max 50% throttles 3s hog (delta<=2400000 usec)",
            0 <= delta <= 2400000, "0 <= delta <= 2400000 usec over ~3s",
            delta)
        sh(g, "kill $(cat /sys/fs/cgroup/t3/cgroup.procs 2>/dev/null) "
              "2>/dev/null; sleep 1; "
              "for p in $(cat /sys/fs/cgroup/t3/cgroup.procs 2>/dev/null); "
              "do echo $p > /sys/fs/cgroup/cgroup.procs 2>/dev/null; done; "
              "rmdir /sys/fs/cgroup/t3 2>/dev/null")
    else:
        checks.append({"check": "guest cpu.max throttling",
                       "expected": "cpu offered (build fact)",
                       "observed": "NOT_TESTED — 'cpu' not offered on "
                                   "candidate-2", "verdict": "MISMATCH"})

    # module policy: MODULES=n — no listing, no loading. Both verbatim.
    rc, out = sh(g, "cat /proc/modules 2>&1; echo MODRC=$?; "
                    "busybox insmod /dev/null /tmp/no.ko 2>&1; "
                    "echo INSMODRC=$?", tag="MODPOL")
    txt = out_text(out)
    modrc = marker(txt, "MODRC", -2)
    insrc = marker(txt, "INSMODRC", -2)
    FACTS["module_policy_probe_raw"] = txt.strip()
    FACTS["proc_modules_rc"] = modrc
    FACTS["busybox_insmod_rc"] = insrc
    chk("module policy: /proc/modules unreadable AND busybox insmod fails",
        modrc != 0 and insrc != 0,
        "MODRC!=0 and INSMODRC!=0 (MODULES=n build fact)",
        {"MODRC": modrc, "INSMODRC": insrc,
         "raw": txt.strip()[:160]})

    # R2 host RSS cost of guest memory (sized to stay alive: 150MiB)
    rc, out = sh(g, "dd if=/dev/zero of=/tmp/shm/f150 bs=1M count=150 "
                    "2>/dev/null; echo dd_rc=$?", timeout=90, tag="R2")
    chk("R2 150MiB tmpfs write fits in 256MiB guest", b"dd_rc=0" in out,
        "dd_rc=0", out_text(out).strip()[-40:])
    time.sleep(0.5)
    vmhwm_kib = -1
    try:
        for line in pathlib.Path(f"/proc/{g.proc.pid}/status").read_text().splitlines():
            if line.startswith("VmHWM"):
                vmhwm_kib = int(line.split()[1])
                FACTS["host_vmhwm_kib_after_150mib_write"] = vmhwm_kib
    except FileNotFoundError:
        pass
    chk("R2 host VmHWM >= 140MiB after 150MiB guest write",
        vmhwm_kib >= 140 * 1024, ">=143360 KiB", vmhwm_kib)
    sh(g, "rm -f /tmp/shm/f150")

    # R4a anon hog (SIZED over managed ~243.7MiB this time: 220M once FIT
    # and proved nothing — recorded): 300M anon MUST die; everyone else
    # must survive. Deterministic allocator: busybox tail -c buffers N
    # bytes in RAM (linear, no awk quadratic copies that starved the
    # single guest CPU in v2's first attempt).
    rc, out = sh(g, "x=$(/bin/busybox head -c 314572800 /dev/urandom); "
                    "echo hog_rc=$?", timeout=120, tag="R4a")
    # the accumulating SHELL is killed: either the subshell death surfaces
    # as frame rc=137 (body empty), or the shell prints hog_rc=137
    hog_killed = (rc == 137) or (b"hog_rc=137" in out)
    chk("R4a 300MiB anon hog OOM-killed (rc=137)", hog_killed,
        "frame rc=137 or hog_rc=137", {"frame_rc": rc,
        "body": out_text(out)[:40]})
    notes.append("hog journey: awk doubling starved the single CPU (v2a); "
                 "busybox tail -c preflights and exits rc=1 without "
                 "allocating (v2b); shell $( ) accumulation is the "
                 "deterministic anon hog (v3)")
    chk("R4a host UML process survives guest OOM", g.proc.poll() is None,
        True, g.proc.poll())
    rc2, out2 = sh(g, "echo alive-after-anon-oom")
    chk("R4a init+channel survive anon OOM",
        rc2 == 0 and b"alive-after-anon-oom" in out2, "echo ok",
        out_text(out2).strip()[:60])

    # R4b shmem oversubscribe probe, up to 3 attempts — RECORDED, not
    # asserted. Observed both ways already: v1 killed init (guest down,
    # the ONLY task candidate), v2 killed dd and the guest stayed up with
    # ~225MiB shmem still resident. History fact recorded either way:
    # killing the ALLOCATOR stops growth; stability afterwards depends on
    # nothing allocating further. Never sold as a guarantee.
    r4b_outcomes = []
    guest_died = False
    sh(g, "mount -o remount,size=400m /tmp/shm 2>/dev/null; rm -f /tmp/shm/big")
    for attempt in (1, 2, 3):
        try:
            rc, out = sh(g, "dd if=/dev/zero of=/tmp/shm/big bs=1M count=300 "
                            "2>/dev/null; echo big_rc=$?", timeout=120,
                         tag=f"R4b#{attempt}")
            txt = out_text(out)
            big = [l for l in txt.splitlines() if l.startswith("big_rc=")]
            big_rc = int(big[0].split("=")[1]) if big else -1
            r4b_outcomes.append(f"attempt{attempt}: big_rc={big_rc}")
            sh(g, "rm -f /tmp/shm/big")
        except (ConnectionError, OSError, AssertionError) as exc:
            # a broken channel means the guest is going down but UML may
            # need ~1s to actually exit (OOM trace, init kill, power-down);
            # never send another frame until the process state settles
            r4b_outcomes.append(f"attempt{attempt}: channel-{type(exc).__name__}")
            try:
                g.proc.wait(timeout=8)
            except Exception:
                pass
            guest_died = True
            break
        if g.proc.poll() is not None:
            guest_died = True
            r4b_outcomes.append(f"attempt{attempt}: guest-exited")
            break
    FACTS["r4b_shmem_oversubscribe_outcomes"] = r4b_outcomes
    chk("R4b shmem oversubscribe behavior recorded (allocator killed or "
        "guest down every time)", all("big_rc=137" in o or "channel-" in o
        or "guest-exited" in o for o in r4b_outcomes),
        "every attempt: big_rc=137|channel-death|guest-exit", r4b_outcomes)

    if not guest_died and g.proc.poll() is None:
        payload = b"/\n/bin/busybox\npoweroff\n-f\n"
        g.host.sendall(b"LSBXQ%08x" % len(payload) + payload)
        rc_boot = g.proc.wait(timeout=30)
        chk("orderly poweroff rc", rc_boot == 0, 0, rc_boot)
    else:
        try:
            g.proc.wait(timeout=10)
        except Exception:
            pass
        notes.append(f"guest ended after R4b, rc={g.proc.returncode}")
    g.close()

def main():
    # harness discipline: a crash mid-experiment is still a verdict — never
    # leave the run as a bare traceback with no JSON
    try:
        run()
    except Exception as exc:
        checks.append({"check": "harness completes without crash",
                       "expected": "no unhandled exception",
                       "observed": f"{type(exc).__name__}: {exc}",
                       "verdict": "MISMATCH"})
    return finalize()


def finalize():
    n_mis = sum(1 for c in checks if c["verdict"] == "MISMATCH")
    out = {
        "experiment": "uml resource semantics on kernel-candidate-2 "
                      "(round-05 §8), pids/cpu/module-policy additions",
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "boundary_notice": "GuestWorkloadHardLimit (memory.max / pids.max / "
            "cpu.max inside the GUEST cgroup v2 hierarchy) is what THIS run "
            "asserts — guest-kernel truth, not a host guarantee. "
            "HostSandboxProcessHardLimit (cgroup hard limits on the host "
            "UML process itself) remains UNSUPPORTED on this host "
            "(cgroupfs ro), exactly the T09-C01 boundary for Native. The "
            "two are never blurred.",
        "verdict": "RESOURCE_SEMANTICS_OK" if n_mis == 0 else
                   "RESOURCE_SEMANTICS_MISMATCH",
        "summary": {"checks_match": sum(1 for c in checks
                                        if c["verdict"] == "MATCH"),
                    "checks_mismatch": n_mis},
        "facts": FACTS,
        "notes": notes,
        "checks": checks,
    }
    dest = RESULTS / "uml-resource-semantics-candidate2.json"
    dest.write_text(json.dumps(out, indent=1))
    print("verdict:", out["verdict"])
    print("checks match/mismatch:", out["summary"]["checks_match"], "/", n_mis)
    print("written:", dest)
    return 0 if n_mis == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
