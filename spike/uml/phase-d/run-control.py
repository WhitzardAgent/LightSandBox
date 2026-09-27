#!/usr/bin/env python3.12
"""T47 phase-d: UML guest-control transport spike (round-04 §21-26).

Channel decision under test: serial ttyS0 over a host fd (ssl0=fd:N) — ADR
0002 §12.1 forbids the console (opens but silently drops userspace writes).
This script is the sole judge: it sends framed exec requests to the guest
handler (lsbx.mode=control in phase-b guest-init.sh), verifies raw facts
(rc/output bytes/sha) host-side, then checks cleanup truth for both an
orderly and a SIGKILL'd guest. Verdict lands in
spike/results/uml-guest-control.json — no text is trusted beyond raw facts.
"""
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
PHASE_B = HERE.parent / "phase-b"
WORK = (HERE.parent / "work").resolve()
PB = WORK / "phase-b"
PD = WORK / "phase-d"
KERNEL = WORK / "src/linux/linux"
CPIO = PB / "phase-b.cpio"
RESULTS = HERE.parent.parent / "results"
HOME_UML = pathlib.Path.home() / ".uml"
PY = "/usr/bin/python3.12"

PD.mkdir(parents=True, exist_ok=True)


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)


def rebuild_initramfs():
    env = dict(os.environ, SPIKE=str(PHASE_B), WORK=str(WORK), PB=str(PB))
    subprocess.run(["bash", str(PHASE_B / "build-initrd.sh")],
                   check=True, env=env)


class Guest:
    def __init__(self, umid, logpath):
        self.umid = umid
        self.host, self.guest = socket.socketpair()
        fd = self.guest.fileno()
        self.logf = open(logpath, "wb")
        self.proc = subprocess.Popen(
            [str(KERNEL), "mem=256M", "con0=fd:0,fd:1", f"ssl0=fd:{fd}",
             f"initrd={CPIO}", f"umid={umid}", "lsbx.mode=control",
             f"lsbx.ocipath={PB / 'oci-cache'}"],
            stdin=subprocess.DEVNULL, stdout=self.logf, stderr=subprocess.STDOUT,
            pass_fds=[fd])
        self.guest.close()  # host keeps self.host only

    def _recv_exact(self, n, deadline):
        buf = b""
        self.host.settimeout(max(0.1, deadline - time.time()))
        while len(buf) < n:
            chunk = self.host.recv(n - len(buf))
            if not chunk:
                raise ConnectionError("serial EOF mid-frame")
            buf += chunk
        return buf

    def exec(self, argv, cwd="/", timeout=30):
        payload = (cwd + "\n" + "\n".join(argv) + "\n").encode()
        self.host.sendall(b"LSBXQ%08x" % len(payload) + payload)
        deadline = time.time() + timeout
        hdr = self._recv_exact(13, deadline)
        assert hdr[:5] == b"LSBXR", f"bad response magic {hdr!r}"
        length = int(hdr[5:], 16)
        resp = self._recv_exact(length, deadline)
        nl = resp.index(b"\n")
        rc = int(resp[3:nl])
        return rc, resp[nl + 1:]

    def wait_ready(self, logpath, timeout=60):
        deadline = time.time() + timeout
        while time.time() < deadline:
            if pathlib.Path(logpath).read_bytes().find(b"LSB_CTL_READY") >= 0:
                return True
            if self.proc.poll() is not None:
                raise RuntimeError(f"guest died early rc={self.proc.returncode}")
            time.sleep(0.2)
        raise TimeoutError("LSB_CTL_READY never appeared")

    def close(self):
        self.host.close()
        self.logf.close()


checks = []


def _rec(v):
    if isinstance(v, bytes):
        return {"len": len(v), "sha256": hashlib.sha256(v).hexdigest(),
                "preview": v[:32].decode("utf-8", errors="backslashreplace")
                if len(v) <= 64 else v[:32].hex()}
    return v


def chk(name, want, obs):
    checks.append({"check": name, "expected": _rec(want), "observed": _rec(obs),
                   "verdict": "MATCH" if want == obs else "MISMATCH"})


def run():
    rebuild_initramfs()

    umid = "lsbxctl1"
    log1 = PD / "console-control.log"
    g = Guest(umid, log1)
    log(f"boot umid={umid} pid={g.proc.pid}")
    g.wait_ready(log1)

    # 1: trivial exec, exact output
    rc, out = g.exec(["/bin/busybox", "echo", "hello-control"])
    chk("exec echo rc", 0, rc)
    chk("exec echo out", b"hello-control\n", out)

    # 2: nonzero exit preserved
    rc, out = g.exec(["/bin/busybox", "false"])
    chk("exec false rc", 1, rc)

    # 3: binary-safe response (raw entropy incl. probable NUL/0x04 bytes)
    rc, out = g.exec(["/bin/busybox", "head", "-c", "300", "/dev/urandom"])
    chk("binary 300B len", 300, len(out))

    # 4: cwd honored
    rc, out = g.exec(["/bin/busybox", "pwd"], cwd="/tmp")
    chk("cwd /tmp pwd", b"/tmp\n", out)

    # 5: large output (64KB zeros) — multi-read reassembly. dd's stats go to
    # stderr and would pollute the deterministic payload ("64+0 records..."),
    # so the guest command silences its own stderr via sh -c. Transport
    # proved byte-exact (this check FAILED first run only because of those
    # stats bytes — fixture determinism bug, not a channel corruption).
    rc, out = g.exec(["/bin/sh", "-c",
                      "dd if=/dev/zero bs=1024 count=64 2>/dev/null"],
                     timeout=60)
    want = b"\x00" * 65536
    chk("large 64KB sha256", hashlib.sha256(want).hexdigest(),
        hashlib.sha256(out).hexdigest())
    chk("large 64KB rc", 0, rc)

    # 6: argv element containing spaces stays one argv (line-framed)
    rc, out = g.exec(["/bin/busybox", "echo", "a b  c"])
    chk("argv spaces preserved", b"a b  c\n", out)

    # orderly shutdown: poweroff -f is UML's power-down verb (reboot -f does
    # NOT power down UML — verified once here: guest survived 30s; phase-a/b
    # evidence used poweroff). It never returns => no response expected.
    payload = b"/\n/bin/busybox\npoweroff\n-f\n"
    g.host.sendall(b"LSBXQ%08x" % len(payload) + payload)
    rc_boot = g.proc.wait(timeout=30)
    chk("orderly poweroff rc", 0, rc_boot)
    time.sleep(0.5)
    residue = (HOME_UML / umid).exists()
    chk("orderly shutdown reaps ~/.uml/<umid>", False, residue)
    g.close()

    # abnormal: SIGKILL mid-life, residue MUST be reapable by a supervisor
    umid2 = "lsbxctl2"
    log2 = PD / "console-control-kill.log"
    g2 = Guest(umid2, log2)
    log(f"boot umid={umid2} pid={g2.proc.pid}")
    g2.wait_ready(log2)
    g2.proc.kill()
    g2.proc.wait(timeout=10)
    time.sleep(0.5)
    rdir = HOME_UML / umid2
    residue_exists = rdir.exists()
    chk("SIGKILL leaves residue (honest)", True, residue_exists)
    reaped = False
    if residue_exists:
        shutil.rmtree(rdir, ignore_errors=True)
        reaped = not rdir.exists()
    chk("supervisor reap removes residue", True, reaped)
    g2.close()

def finalize():
    n_mis = sum(1 for c in checks if c["verdict"] == "MISMATCH")
    out = {
        "experiment": "uml phase-d guest-control transport (round-04 21-26)",
        "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "channel": "ssl0=fd:N over host socketpair; console NEVER used "
                   "(ADR 0002 12.1: console silently drops userspace writes)",
        "framing_v1": "REQ 'LSBXQ'+%08x len+payload(cwd\\n argv lines); "
                      "RESP 'LSBXR'+%08x len+('rc=N\\n'+raw output). "
                      "argv elements must not contain \\n/NUL (shell line "
                      "framing); response payload binary-safe via file handoff",
        "mconsole": "NOT the exec path (management socket: halt/sysrq "
                    "candidates only); uml_mconsole tool not built in this "
                    "spike — lifecycle-side evaluation stays open",
        "verdict": "GUEST_CONTROL_OK" if n_mis == 0 else "GUEST_CONTROL_MISMATCH",
        "summary": {"checks_match": sum(1 for c in checks if c["verdict"] == "MATCH"),
                    "checks_mismatch": n_mis},
        "checks": checks,
    }
    dest = RESULTS / "uml-guest-control.json"
    dest.write_text(json.dumps(out, indent=1))
    print("verdict:", out["verdict"])
    print("checks match/mismatch:", out["summary"]["checks_match"], "/", n_mis)
    print("written:", dest)
    return 0 if n_mis == 0 else 1


def main():
    # harness discipline (round-05 review fix): a crash mid-experiment is
    # still a verdict — never leave the run as a bare traceback with no JSON
    try:
        run()
    except Exception as exc:
        checks.append({"check": "harness completes without crash",
                       "expected": "no unhandled exception",
                       "observed": f"{type(exc).__name__}: {exc}",
                       "verdict": "MISMATCH"})
    return finalize()


if __name__ == "__main__":
    sys.exit(main())
