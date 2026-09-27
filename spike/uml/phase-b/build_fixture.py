#!/usr/bin/env python3.12
"""T47 OCI fidelity fixture builder (host side, stdlib only).

Emits a deterministic OCI Image Layout at
  spike/uml/work/phase-b/oci-cache/fidelity/
plus expected.json (hand-declared intents the guest reconciliation is scored
against). Determinism (fixed mtime) makes digests stable across rebuilds.

Coverage per the T47 card: root-owned file, uid 1000/gid 1000 file,
uid 999/gid 999 file, modes 0600/0755, setuid bit, hardlink (in-layer and
cross-layer), symlink, layer overwrite, layer whiteout deletion, directory
ownership, ENTRYPOINT/CMD/ENV/WORKDIR/USER 1000, and a runnable rootfs
(busybox inside the image, because the runtime phase boots the base disk).
"""
import hashlib
import io
import json
import os
import posixpath
import shutil
import sys
import tarfile

HERE = os.path.dirname(os.path.abspath(__file__))
WORK = os.path.normpath(os.path.join(HERE, "..", "work", "phase-b"))
OCI = os.path.join(WORK, "oci-cache", "fidelity")
BUSYBOX = os.path.normpath(os.path.join(HERE, "..", "work", "rootfs", "bin", "busybox"))
MTIME = 1_700_000_000  # fixture-wide deterministic mtime

PASSWD = b"root:x:0:0:root:/root:/bin/sh\nuser:x:1000:1000:user:/home/user:/bin/sh\nsvc:x:999:999:svc:/srv/data:/bin/sh\n"
GROUP = b"root:x:0:\nuser:x:1000:\nsvc:x:999:\n"
MOTD_V1 = b"config-v1\n"
MOTD_V2 = b"config-v2\n"  # overwrites MOTD_V1 in layer 2
SECRET = b"root-only\n"
USER_NOTE = b"user-note\n"
LINKED = b"linked-content\n"
SVC = b"""#!/bin/sh
echo "SVC_ARGV=$*"
echo "SVC_PWD=$(pwd)"
echo "SVC_ID=$(id)"
echo "SVC_ENV_HOME=$SVC_HOME"
echo "SVC_MARK=$MARK"
"""
SVC_FLAG = b"layer2-flag\n"
SUID_ID = b"#!/bin/sh\necho SUID_RAN\n"  # only MODE 4755 is under test

L1 = [
    # (path, kind, data/link, uid, gid, mode)
    ("etc", "dir", None, 0, 0, 0o755),
    ("etc/passwd", "file", PASSWD, 0, 0, 0o644),
    ("etc/group", "file", GROUP, 0, 0, 0o644),
    ("etc/motd.conf", "file", MOTD_V1, 0, 0, 0o644),
    ("root", "dir", None, 0, 0, 0o700),
    ("root/secret", "file", SECRET, 0, 0, 0o600),
    ("home", "dir", None, 0, 0, 0o755),
    ("home/user", "dir", None, 1000, 1000, 0o755),
    ("home/user/note", "file", USER_NOTE, 1000, 1000, 0o600),
    ("srv", "dir", None, 0, 0, 0o755),
    ("srv/data", "dir", None, 999, 999, 0o755),  # WORKDIR: 999-owned but
                                                  # world-enterable so USER 1000 can cd
    ("opt", "dir", None, 0, 0, 0o755),
    ("opt/svc", "dir", None, 999, 999, 0o755),
    ("opt/svc/bin", "dir", None, 999, 999, 0o755),
    ("opt/svc/bin/svc", "file", SVC, 999, 999, 0o755),
    ("usr", "dir", None, 0, 0, 0o755),
    ("usr/bin", "dir", None, 0, 0, 0o755),
    ("usr/bin/suid-id", "file", SUID_ID, 0, 0, 0o4755),  # setuid bit
    ("usr/bin/svctool", "symlink", "/opt/svc/bin/svc", 0, 0, 0o777),
    ("bin", "dir", None, 0, 0, 0o755),
    ("bin/busybox", "file", b"__BUSYBOX__", 0, 0, 0o755),
    ("bin/sh", "symlink", "busybox", 0, 0, 0o777),
    ("bin/id", "symlink", "busybox", 0, 0, 0o777),
    ("bin/su", "symlink", "busybox", 0, 0, 0o777),
    ("a", "dir", None, 0, 0, 0o755),
    ("a/x", "file", LINKED, 0, 0, 0o644),
    ("a/y", "hardlink", "a/x", 0, 0, 0o644),  # same inode as a/x
    ("tmp", "dir", None, 0, 0, 0o1777),
    ("tmp/gone", "file", b"deleted-in-layer-2\n", 0, 0, 0o644),
    ("deep", "dir", None, 0, 0, 0o755),
    ("deep/a", "dir", None, 1000, 999, 0o750),
    ("deep/a/b", "dir", None, 1000, 999, 0o750),
    ("deep/a/b/c", "dir", None, 1000, 999, 0o700),
    ("deep/a/b/c/leaf", "file", b"deep\n", 1000, 999, 0o600),
]
L2 = [
    ("etc/motd.conf", "file", MOTD_V2, 0, 0, 0o644),        # overwrite
    ("tmp/.wh.gone", "file", b"", 0, 0, 0o644),             # whiteout deletes tmp/gone
    ("opt/svc/bin/flag", "file", SVC_FLAG, 999, 999, 0o400),
    ("a/z", "hardlink", "a/x", 0, 0, 0o644),                # cross-layer hardlink
]

CONFIG = {
    "architecture": "amd64",
    "os": "linux",
    "config": {
        "Env": ["SVC_HOME=/opt/svc", "PATH=/usr/bin:/bin", "MARK=round04"],
        "Entrypoint": ["/opt/svc/bin/svc"],
        "Cmd": ["--serve", "--default"],
        "WorkingDir": "/srv/data",
        "User": "1000",
    },
    # rootfs filled in after digests are known
}


def sha(b):
    return hashlib.sha256(b).hexdigest()


def add_entry(tf, path, kind, data, uid, gid, mode):
    ti = tarfile.TarInfo(path)
    ti.mtime = MTIME
    ti.uid, ti.gid = uid, gid
    ti.uname, ti.gname = str(uid), str(gid)
    if kind == "dir":
        ti.type = tarfile.DIRTYPE
        ti.mode = mode
        tf.addfile(ti)
    elif kind == "symlink":
        ti.type = tarfile.SYMTYPE
        ti.linkname = data
        ti.mode = mode
        tf.addfile(ti)
    elif kind == "hardlink":
        ti.type = tarfile.LNKTYPE
        ti.linkname = data
        ti.mode = mode
        tf.addfile(ti)
    else:
        ti.type = tarfile.REGTYPE
        ti.mode = mode
        ti.size = len(data)
        tf.addfile(ti, io.BytesIO(data))


def layer_bytes(entries):
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as tf:
        for path, kind, data, uid, gid, mode in entries:
            if data == b"__BUSYBOX__":
                data = open(BUSYBOX, "rb").read()
            add_entry(tf, path, kind, data, uid, gid, mode)
    return buf.getvalue()


def write_blob(blobs, payload):
    d = sha(payload)
    p = os.path.join(blobs, d)
    with open(p, "wb") as fh:
        fh.write(payload)
    return d, len(payload)


def main():
    if os.path.exists(WORK):
        shutil.rmtree(WORK)
    blobs = os.path.join(OCI, "blobs", "sha256")
    os.makedirs(blobs)

    l1 = layer_bytes(L1)
    l2 = layer_bytes(L2)
    d1, s1 = write_blob(blobs, l1)
    d2, s2 = write_blob(blobs, l2)

    cfg = dict(CONFIG)
    cfg["rootfs"] = {
        "type": "layers",
        "diff_ids": [f"sha256:{d1}", f"sha256:{d2}"],
    }
    cfg_bytes = json.dumps(cfg, indent=2).encode()
    cd, cs = write_blob(blobs, cfg_bytes)

    manifest = {
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": f"sha256:{cd}",
            "size": cs,
        },
        "layers": [
            {"mediaType": "application/vnd.oci.image.layer.v1.tar",
             "digest": f"sha256:{d1}", "size": s1},
            {"mediaType": "application/vnd.oci.image.layer.v1.tar",
             "digest": f"sha256:{d2}", "size": s2},
        ],
    }
    m_bytes = json.dumps(manifest, indent=2).encode()
    md, ms = write_blob(blobs, m_bytes)
    with open(os.path.join(OCI, "index.json"), "w") as fh:
        json.dump({
            "schemaVersion": 2,
            "manifests": [{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": f"sha256:{md}",
                "size": ms,
                "annotations": {"org.opencontainers.image.ref.name": "latest"},
            }],
        }, fh, indent=2)
    with open(os.path.join(OCI, "oci-layout"), "w") as fh:
        json.dump({"imageLayoutVersion": "1.0.0"}, fh)

    bb = open(BUSYBOX, "rb").read()
    expected = {
        "fixture_maker": "spike/uml/phase-b/build_fixture.py (deterministic mtime %d)" % MTIME,
        "manifest_digest": f"sha256:{md}",
        "config_digest": f"sha256:{cd}",
        # the runtime-config file the builder writes to the base disk is
        # umoci-generated; its expected sha256 is NOT known here (first build
        # records it as observed, later runs compare stability).
        "entries": [
            {"path": "/etc/passwd", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "644", "sha256": sha(PASSWD)},
            {"path": "/etc/group", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "644", "sha256": sha(GROUP)},
            {"path": "/etc/motd.conf", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "644", "sha256": sha(MOTD_V2), "note": "layer-2 overwrite must win"},
            {"path": "/root/secret", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "600", "sha256": sha(SECRET)},
            {"path": "/home/user", "exists": True, "type": "dir", "uid": 1000, "gid": 1000, "mode": "755"},
            {"path": "/home/user/note", "exists": True, "type": "file", "uid": 1000, "gid": 1000, "mode": "600", "sha256": sha(USER_NOTE)},
            {"path": "/srv/data", "exists": True, "type": "dir", "uid": 999, "gid": 999, "mode": "755", "note": "WORKDIR target for USER 1000"},
            {"path": "/opt/svc/bin/svc", "exists": True, "type": "file", "uid": 999, "gid": 999, "mode": "755", "sha256": sha(SVC)},
            {"path": "/usr/bin/suid-id", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "4755", "sha256": sha(SUID_ID), "note": "setuid bit"},
            {"path": "/usr/bin/svctool", "exists": True, "type": "symlink", "link": "/opt/svc/bin/svc"},
            {"path": "/bin/sh", "exists": True, "type": "symlink", "link": "busybox"},
            {"path": "/bin/id", "exists": True, "type": "symlink", "link": "busybox"},
            {"path": "/bin/su", "exists": True, "type": "symlink", "link": "busybox"},
            {"path": "/bin/busybox", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "755", "sha256": sha(bb)},
            {"path": "/a/x", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "644", "sha256": sha(LINKED)},
            {"path": "/a/y", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "644", "sha256": sha(LINKED), "hardlink_to": "/a/x"},
            {"path": "/a/z", "exists": True, "type": "file", "uid": 0, "gid": 0, "mode": "644", "sha256": sha(LINKED), "hardlink_to": "/a/x", "note": "cross-layer hardlink"},
            {"path": "/tmp/gone", "exists": False, "note": "layer-2 whiteout deletion"},
            {"path": "/opt/svc/bin/flag", "exists": True, "type": "file", "uid": 999, "gid": 999, "mode": "400", "sha256": sha(SVC_FLAG)},
            {"path": "/deep/a/b/c", "exists": True, "type": "dir", "uid": 1000, "gid": 999, "mode": "700"},
            {"path": "/deep/a/b/c/leaf", "exists": True, "type": "file", "uid": 1000, "gid": 999, "mode": "600", "sha256": sha(b"deep\n")},
        ],
        "runtime_expect": {
            "cwd": "/srv/data",
            "uid": 1000,
            "argv_suffix": "--serve --default",
            "argv0": "/opt/svc/bin/svc",
            "env": {"SVC_HOME": "/opt/svc", "MARK": "round04"},
            "gid_note": ("umoci raw runtime-config derives gid=0 for numeric "
                         "User without image-passwd lookup; the guest su "
                         "resolves via the image /etc/passwd to gid=1000 "
                         "(runc-consistent). Recorded, not scored."),
        },
    }
    with open(os.path.join(WORK, "expected.json"), "w") as fh:
        json.dump(expected, fh, indent=2)

    # guest probe list (paths only; expectations stay host-side). Written into
    # the EXPOSED cache root — hostfs mounts oci-cache/ as /oci, so the guest
    # sees this as /oci/probe-paths.txt.
    with open(os.path.join(WORK, "oci-cache", "probe-paths.txt"), "w") as fh:
        for e in expected["entries"]:
            fh.write(e["path"] + "\n")
    print("fixture written:", OCI)
    print("manifest sha256:", md)
    print("config   sha256:", cd)
    print("expected.json + probe-paths.txt written under", WORK)


if __name__ == "__main__":
    main()
