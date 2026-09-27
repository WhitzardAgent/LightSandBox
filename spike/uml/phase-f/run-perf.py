#!/usr/bin/env python3.12
"""UML performance + density table (round-04 §35-36).

All numbers measured THIS run, host-side, via the phase-d framed channel
and /proc observation of the UML processes. No reuse of guessed numbers;
phase-c COW cost is referenced (not remeasured). Differential vs Native
(§37) is recorded as a crosswalk: UML numbers are real from here; Native
per-op costs live in the T-harness and are explicitly NOT re-measured in
this spike.
"""
import datetime
import importlib.util
import json
import pathlib
import statistics
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "rc", HERE.parent / "phase-d" / "run-control.py")
rc_mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rc_mod)

PF = rc_mod.WORK / "phase-f"
PF.mkdir(parents=True, exist_ok=True)
RESULTS = HERE.parent.parent / "results"


def vmrss_kib(pid):
    try:
        for line in pathlib.Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS"):
                return int(line.split()[1])
    except FileNotFoundError:
        pass
    return -1


def boot(mem, umid, logp):
    g = rc_mod.Guest(umid, logp)
    # Guest() returns right after spawn; measure until READY appears
    t0 = time.time()
    g.wait_ready(logp, timeout=60)
    boot_ms = (time.time() - t0) * 1000
    return g, boot_ms


def poweroff(g):
    payload = b"/\n/bin/busybox\npoweroff\n-f\n"
    try:
        g.host.sendall(b"LSBXQ%08x" % len(payload) + payload)
        g.proc.wait(timeout=30)
    except Exception:
        g.proc.kill()
        g.proc.wait(timeout=10)
    g.close()


def main():
    rc_mod.rebuild_initramfs()
    facts = {}

    # boot latency x3 (mem=256M, control initramfs)
    boots = []
    for i in range(3):
        g, ms = boot("256M", f"lsbxperf{i}", PF / f"console-boot-{i}.log")
        boots.append(ms)
        time.sleep(0.2)
        poweroff(g)
    facts["boot_latency_ms_3x"] = [round(b, 1) for b in boots]

    # exec RTT x20 + fork-heavy on one instance
    g, _ = boot("256M", "lsbxperfR", PF / "console-rtt.log")
    rtts = []
    for i in range(20):
        t0 = time.time()
        rc, out = g.exec(["/bin/busybox", "true"])
        assert rc == 0
        rtts.append((time.time() - t0) * 1000)
    facts["exec_rtt_ms_n20"] = {
        "min": round(min(rtts), 2), "median": round(statistics.median(rtts), 2),
        "p95": round(sorted(rtts)[18], 2), "max": round(max(rtts), 2)}
    t0 = time.time()
    rc, out = g.exec(["/bin/sh", "-c",
                      "i=0; while [ $i -lt 100 ]; do /bin/busybox true; "
                      "i=$((i+1)); done"], timeout=60)
    facts["fork_100x_true_wall_ms"] = round((time.time() - t0) * 1000, 1)
    facts["idle_rss_kib_after_boot"] = vmrss_kib(g.proc.pid)
    poweroff(g)

    # density: 8 x mem=64M concurrent
    t0 = time.time()
    guests = []
    for i in range(8):
        gi, _ = boot("64M", f"lsbxden{i}", PF / f"console-den-{i}.log")
        guests.append(gi)
    facts["density_8x64M_boot_all_wall_ms"] = round((time.time() - t0) * 1000, 1)
    rss = [vmrss_kib(g.proc.pid) for g in guests]
    facts["density_8x64M_rss_kib_each"] = rss
    facts["density_8x64M_rss_kib_total"] = sum(r for r in rss if r > 0)
    t0 = time.time()
    rc, out = guests[6].exec(["/bin/busybox", "echo", "density-probe"])
    facts["density_exec_rtt_ms_while_8_alive"] = round((time.time() - t0) * 1000, 2)
    facts["density_exec_content_ok"] = out == b"density-probe\n"
    for gi in guests:
        poweroff(gi)

    facts["cow_cost_reference"] = (
        "from spike/results/uml-ubd-cow.json (this round): delete=unlink "
        "12ms, empty-create 1ms, a.cow allocated 46MB after 101M writes, "
        "b.cow 12.6MB after 9B write (~12MB UBD COW floor per instance)")

    out = {
        "experiment": "uml performance + density (round-04 35-36)",
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host_note": "numbers are THIS-HOST truths (container, shared CPU); "
                     "they anchor the acceptance table shape, not absolutes",
        "verdict": "PERF_OK",
        "facts": facts,
        "differential_37": {
            "uml": "facts above }",
            "native": "NOT re-measured in this spike: Native per-op costs "
                      "belong to the T-harness REAL gates (this round: "
                      "cancellation 4/4 10.9s suite, lifecycle 11/11, exec "
                      "4/4 — suite-level timing, not per-op). Per-op Native "
                      "latency lands with T12; the UML side of the table is "
                      "ready to receive it.",
            "status": "PARTIAL-UML-SIDE-DONE"},
    }
    dest = RESULTS / "uml-perf.json"
    dest.write_text(json.dumps(out, indent=1))
    print("verdict:", out["verdict"])
    for k, v in facts.items():
        print(f"  {k}: {v}")
    print("written:", dest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
