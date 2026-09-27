#!/usr/bin/env python3.12
"""T47-C02 reconciliation: COW leg probes -> spike/results/uml-ubd-cow.json.

Isolation law (scored host-side from guest-reported presence/sha facts):
  leg A  writes: a-marker, motd.conf "v2+A", flag deleted, big1/big100
  leg B  (fresh cow): NONE of A's writes may be visible; B writes /cow-b-marker
  leg A2 (A's cow): ALL of A's writes persisted exactly
  leg C  (fresh cow): same clean-world expectation as B; C writes /cow-c-marker
Plus: base sha256 equal across all legs; cost numbers recorded verbatim.
"""
import datetime
import hashlib
import json
import pathlib
import sys

COWDIR = pathlib.Path(sys.argv[1])
SHA0 = sys.argv[2]
SHAS_EQUAL = sys.argv[3] == "true"
CREATE_A_MS, DELETE_MS, CREATE_EMPTY_MS = (int(sys.argv[4]), int(sys.argv[5]), int(sys.argv[6]))
SIZE_A, ALLOC_A, SIZE_B, ALLOC_B = (int(sys.argv[7]), int(sys.argv[8]), int(sys.argv[9]), int(sys.argv[10]))

V2 = hashlib.sha256(b"config-v2\n").hexdigest()
V2A = hashlib.sha256(b"config-v2\n+A\n").hexdigest()
MARKER_A = hashlib.sha256(b"marker-a\n").hexdigest()
MARKER_B = hashlib.sha256(b"marker-b\n").hexdigest()
MARKER_C = hashlib.sha256(b"marker-c\n").hexdigest()


def probes(log):
    out = {}
    for line in pathlib.Path(log).read_text(errors="replace").splitlines():
        if line.startswith("COW|"):
            _, path, state, sha = line.split("|", 3)
            out[path] = (state, sha)
    return out


A = probes(COWDIR / "console-A.log")
B = probes(COWDIR / "console-B.log")
A2 = probes(COWDIR / "console-A2.log")
C = probes(COWDIR / "console-C.log")

checks = []


def chk(name, want, obs):
    checks.append({"check": name, "expected": want, "observed": obs,
                   "verdict": "MATCH" if want == obs else "MISMATCH"})


def presence(leg, name, path, want_present, want_sha=None):
    state, sha = leg.get(path, ("missing-probe", "."))
    chk(f"{name}: {path} presence", "present" if want_present else "absent", state)
    if want_present and want_sha:
        chk(f"{name}: {path} content", want_sha[:16], sha[:16])


# A's own view right after writing: everything visible
presence(A, "A", "/cow/a-marker", True, MARKER_A)
presence(A, "A", "/etc/motd.conf", True, V2A)
presence(A, "A", "/opt/svc/bin/flag", False)
presence(A, "A", "/cow/big100", True)

# B: fresh cow must be clean of A
presence(B, "B", "/cow/a-marker", False)
presence(B, "B", "/cow/big1", False)
presence(B, "B", "/cow/big100", False)
presence(B, "B", "/etc/motd.conf", True, V2)
presence(B, "B", "/opt/svc/bin/flag", True)
presence(B, "B", "/cow-b-marker", True, MARKER_B)

# A2: A's cow revisited — persistence (B's marker must NOT leak in)
presence(A2, "A2", "/cow/a-marker", True, MARKER_A)
presence(A2, "A2", "/cow/big1", True)
presence(A2, "A2", "/cow/big100", True)
presence(A2, "A2", "/etc/motd.conf", True, V2A)
presence(A2, "A2", "/opt/svc/bin/flag", False)
presence(A2, "A2", "/cow-b-marker", False)

# C: fresh cow, clean like B (must see neither A's nor B's writes)
presence(C, "C", "/cow/a-marker", False)
presence(C, "C", "/cow-b-marker", False)
presence(C, "C", "/cow-c-marker", True, MARKER_C)
presence(C, "C", "/etc/motd.conf", True, V2)

n_mis = sum(1 for c in checks if c["verdict"] == "MISMATCH")
n_ok = sum(1 for c in checks if c["verdict"] == "MATCH")

out = {
    "experiment": "uml phase-c UBD COW correctness + cost (T47-C02)",
    "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "base_disk_sha256_all_legs_equal": SHAS_EQUAL,
    "base_sha256": SHA0,
    "cow_syntax": "ubd0=<cow-file>,<backing-file> (official UBD driver)",
    "verdict": "COW_OK" if (n_mis == 0 and SHAS_EQUAL) else "COW_MISMATCH",
    "summary": {"checks_match": n_ok, "checks_mismatch": n_mis},
    "checks": checks,
    "cost": {
        "cow_a_first_boot_wall_ms_includes_writes_1M_100M": CREATE_A_MS,
        "cow_delete_file_ms": DELETE_MS,
        "cow_create_empty_file_ms": CREATE_EMPTY_MS,
        "a_cow_logical_bytes": SIZE_A,
        "a_cow_allocated_bytes_after_101M_writes": ALLOC_A,
        "b_cow_allocated_bytes_after_b_marker_write": ALLOC_B,
        "base_disk_logical_bytes": 100663296,
        "note": "allocated bytes from stat(2) st_blocks*512 of the cow file; "
                "delete latency is unlink(2) of the cow file on the host fs. "
                "first-boot wall includes kernel boot (~1.3s measured in phase-b).",
    },
}
dest = pathlib.Path(__file__).resolve().parents[2] / "results" / "uml-ubd-cow.json"
dest.parent.mkdir(parents=True, exist_ok=True)
dest.write_text(json.dumps(out, indent=1))
print("verdict:", out["verdict"])
print("checks match/mismatch:", n_ok, "/", n_mis)
print("written:", dest)
