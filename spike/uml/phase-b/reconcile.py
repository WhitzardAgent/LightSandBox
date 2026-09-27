#!/usr/bin/env python3.12
"""T47 reconciliation: guest-reported FID/RT markers vs expected.json.

Emits spike/results/uml-oci-fidelity.json. Per-field verdicts are one of
MATCH / MISMATCH / UNSUPPORTED / NOT_TESTED (card §16: no "looks correct").
The observed value is ALWAYS recorded next to the expected one so a mismatch
is self-describing.
"""
import datetime
import json
import pathlib
import re
import sys

PB = pathlib.Path(sys.argv[1])
KSHA = sys.argv[2]
RC_B = int(sys.argv[3])
RC_R = int(sys.argv[4])

expected = json.loads((PB / "expected.json").read_text())
console_b = (PB / "console-build.log").read_text(errors="replace")
console_r = (PB / "console-runtime.log").read_text(errors="replace")

fields = {}
for m in re.finditer(r"^FID\|(.*)$", console_b, re.M):
    parts = m.group(1).split("|")
    # FID|path|ABSENT
    # FID|path|OK|uid|gid|mode|type|ino|sha-or-target
    fields[parts[0]] = parts[1:]

mark = lambda name: (re.search(rf"^{re.escape(name)}", console_b, re.M) is not None)

entries = []
n_match = n_mismatch = 0
inodes = {}
for e in expected["entries"]:
    got = fields.get(e["path"])
    rec = {"path": e["path"], "fields": {}}
    if got is None:
        rec["verdict"] = "NOT_TESTED"
        entries.append(rec)
        continue
    if not e.get("exists", True):
        v = "MATCH" if got[0] == "ABSENT" else "MISMATCH"
        rec["fields"]["absence"] = {"expected": "ABSENT", "observed": got[0], "verdict": v}
    else:
        ok = got[0] == "OK"
        rec["fields"]["presence"] = {"expected": "present", "observed": got[0],
                                     "verdict": "MATCH" if ok else "MISMATCH"}
        if ok:
            uid, gid, mode, typ, ino, extra = got[1:]
            inodes.setdefault(e["path"], ino)
            want_type = {"file": "file", "dir": "dir", "symlink": "symlink"}[e["type"]]
            checks = {
                "type": (want_type, typ),
            }
            if e["type"] != "symlink":
                checks["uid"] = (str(e["uid"]), uid)
                checks["gid"] = (str(e["gid"]), gid)
                checks["mode"] = (e["mode"], mode)
            if e["type"] == "file" and "sha256" in e:
                checks["content_sha256"] = (e["sha256"], extra)
            if e["type"] == "symlink":
                checks["link_target"] = (e["link"], extra)
            for name, (want, obs) in checks.items():
                rec["fields"][name] = {"expected": want, "observed": obs,
                                       "verdict": "MATCH" if want == obs else "MISMATCH"}
    for f in rec["fields"].values():
        if f.get("verdict") == "MISMATCH":
            n_mismatch += 1
        elif f.get("verdict") == "MATCH":
            n_match += 1
    rec["verdict"] = ("MATCH" if all(f.get("verdict") == "MATCH" for f in rec["fields"].values())
                      else "MISMATCH")
    if "note" in e:
        rec["note"] = e["note"]
    entries.append(rec)

# hardlink identity: inode equality for pairs declared hardlink_to
hl = []
for e in expected["entries"]:
    tgt = e.get("hardlink_to")
    if tgt and inodes.get(e["path"]) and inodes.get(tgt):
        v = "MATCH" if inodes[e["path"]] == inodes[tgt] else "MISMATCH"
        hl.append({"path": e["path"], "target": tgt,
                   "inode_path": inodes[e["path"]], "inode_target": inodes[tgt],
                   "verdict": v})
        (globals().__setitem__("n_match", n_match + 1) if v == "MATCH"
         else globals().__setitem__("n_mismatch", n_mismatch + 1))

runtime = {}
rtexp = expected["runtime_expect"]
def lens(name, rx):
    m = re.search(rx, console_b + "\n" + console_r, re.M)
    return m.group(1) if m else None

svc = {}
for key in ["SVC_ARGV", "SVC_PWD", "SVC_ID", "SVC_ENV_HOME", "SVC_MARK"]:
    svc[key] = lens(key, rf"^{key}=(.*)$")
runtime["cwd"] = {"expected": rtexp["cwd"], "observed": svc.get("SVC_PWD"),
                  "verdict": "MATCH" if (svc.get("SVC_PWD") or "") == rtexp["cwd"] else "MISMATCH"}
obs_argv = svc.get("SVC_ARGV") or ""
runtime["argv_from_entrypoint_plus_cmd"] = {
    "expected_suffix": rtexp["argv_suffix"], "observed": obs_argv,
    "verdict": "MATCH" if obs_argv.strip() == rtexp["argv_suffix"] else "MISMATCH"}
obs_id = svc.get("SVC_ID") or ""
runtime["uid"] = {"expected": rtexp["uid"], "observed": obs_id,
                  "verdict": "MATCH" if f"uid={rtexp['uid']}(" in obs_id else "MISMATCH"}
for k, want in rtexp["env"].items():
    obs = svc.get("SVC_ENV_HOME" if k == "SVC_HOME" else "SVC_MARK") if k in ("SVC_HOME", "MARK") else None
    runtime[f"env_{k}"] = {"expected": want, "observed": obs,
                           "verdict": "MATCH" if obs == want else "MISMATCH"}

boot = {
    "hostfs_read_only": mark("LSB_HOSTFS_RO_OK") and not mark("LSB_HOSTFS_RO_Violated"),
    "hostfs_mount": mark("LSB_HOSTFS_MOUNT_OK"),
    "ubd_mount": mark("LSB_UBD_MOUNT_OK"),
    "umoci_unpack": mark("LSB_UNPACK_OK"),
    "runtime_config_write": mark("LSB_RTCONFIG_OK"),
    "build_phase_done": mark("LSB_PHASE_BUILD_DONE"),
    "runtime_mount": "LSB_RT_MOUNT_OK" in console_r,
    "runtime_phase_done": "LSB_PHASE_RUNTIME_DONE" in console_r,
    "observed_config_sha_build": lens("LSB_CONFIGSHA", r"^LSB_CONFIGSHA=(\w+)"),
    "observed_config_sha_runtime": lens("LSB_RT_CONFIGSHA", r"^LSB_RT_CONFIGSHA=(\w+)"),
}
rt_verdicts = [v["verdict"] for v in runtime.values()]
verdict = "FIDELITY_OK"
if n_mismatch == 0 and all(v == "MATCH" for v in rt_verdicts) and all(
        boot[k] for k in ["hostfs_read_only", "umoci_unpack", "build_phase_done",
                          "runtime_mount", "runtime_phase_done"]):
    verdict = "FIDELITY_OK"
elif any(boot[k] is False for k in ["hostfs_read_only", "umoci_unpack", "build_phase_done"]):
    verdict = "MATERIALIZATION_FAILED"
else:
    verdict = "FIDELITY_MISMATCH"

out = {
    "experiment": "uml phase-b OCI materialization fidelity (T47-C01)",
    "utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "kernel_binary_sha256": KSHA,
    "build_boot_rc": RC_B,
    "runtime_boot_rc": RC_R,
    "manifest_digest": expected["manifest_digest"],
    "config_digest": expected["config_digest"],
    "hostfs_boundary": "read-only hostfs exposing ONLY spike/uml/work/phase-b/oci-cache (dedicated dir)",
    "verdict": verdict,
    "summary": {"fields_match": n_match, "fields_mismatch": n_mismatch,
                "entries": len(entries), "runtime_checks": len(runtime)},
    "entries": entries,
    "hardlink_identity": hl,
    "runtime": runtime,
    "boot_markers": boot,
}
dest = pathlib.Path(__file__).resolve().parents[2] / "results" / "uml-oci-fidelity.json"
dest.parent.mkdir(parents=True, exist_ok=True)
dest.write_text(json.dumps(out, indent=1))
print("verdict:", verdict)
print("fields match/mismatch:", n_match, "/", n_mismatch)
print("runtime:", {k: v["verdict"] for k, v in runtime.items()})
print("written:", dest)
