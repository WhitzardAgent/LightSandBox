#!/usr/bin/env python3
"""Portable Rootless Runtime Spike lab runner (feasibility-only; see README).

Subcommands: fixtures, unpack, firstrun, p1, p2, uids, rootfs, isolation, perf.
All measurements are argv
runs with timing; evidence lands in spike/results/<exp>.json.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
TOOLS = Path("/opt/toolchains/lightsandbox/bin")
UMOCI = TOOLS / "umoci"
RUNC = TOOLS / "runc"
# Runtime material must be reachable by the unprivileged spike user (65534);
# /root and $HOME/.local are not traversable from other uids.
STATE = Path("/tmp/lsbx-spike")
RESULTS = REPO / "spike" / "results"


def run(argv, timeout=600, cwd=None, env=None):
    t0 = time.monotonic()
    proc = subprocess.run([str(a) for a in argv], capture_output=True, text=True,
                          errors="replace", timeout=timeout, cwd=cwd, env=env)
    ms = int((time.monotonic() - t0) * 1000)
    return {"argv": [str(a) for a in argv], "exit": proc.returncode, "ms": ms,
            "stdout": proc.stdout[-4000:], "stderr": proc.stderr[-4000:]}


NOBODY_UID = 65534
NOBODY_GID = 65534
LSBX_RT = Path("/tmp/lsbx-runtime")
LSBX_NOBODY_HOME = Path("/tmp/lsbx-nobody-home")


def prepare_nobody_context():
    LSBX_RT.mkdir(parents=True, exist_ok=True)
    LSBX_NOBODY_HOME.mkdir(parents=True, exist_ok=True)
    for p in (LSBX_RT, LSBX_NOBODY_HOME):
        os.chown(p, NOBODY_UID, NOBODY_GID)
        os.chmod(p, 0o700)


def run_as(uid, gid, argv, timeout=600, cwd=None, env_extra=None):
    """Run argv as an unprivileged uid/gid (preexec drop in the forked child),
    mirroring the PORTABLE_ROOTLESS target: the runtime executes as the real
    unprivileged user, not as the nested container's uid-0."""
    def drop():
        os.setgid(gid)
        os.setuid(uid)
        os.environ.pop("HOME", None)
    env = {"HOME": str(LSBX_NOBODY_HOME), "XDG_RUNTIME_DIR": str(LSBX_RT),
           "PATH": "/usr/local/bin:/usr/bin:/bin"}
    if env_extra:
        env.update(env_extra)
    t0 = time.monotonic()
    proc = subprocess.run([str(a) for a in argv], capture_output=True, text=True,
                          errors="replace", timeout=timeout, cwd=cwd, env=env,
                          preexec_fn=drop)
    ms = int((time.monotonic() - t0) * 1000)
    return {"argv": [str(a) for a in argv], "exit": proc.returncode, "ms": ms,
            "run_as": f"{uid}:{gid}", "stdout": proc.stdout[-4000:],
            "stderr": proc.stderr[-4000:]}


def run_forcing(argv, what, **kw):
    res = run(argv, **kw)
    if res["exit"] != 0:
        raise SystemExit(f"spike setup failed at {what}: {json.dumps(res, ensure_ascii=False)[:800]}")
    return res


class Evidence:
    def __init__(self, name):
        self.name = name
        self.data = {"experiment": name, "started_at": datetime.now(timezone.utc).isoformat(),
                     "host": {"kernel": os.uname().release, "uid": os.getuid()},
                     "tools": tool_facts(), "steps": []}

    def step(self, title, result, note="", extra=None):
        entry = {"title": title, "note": note, "result": result}
        if extra:
            entry["extra"] = extra
        self.data["steps"].append(entry)
        return entry

    def finish(self, verdict):
        self.data["verdict"] = verdict
        RESULTS.mkdir(parents=True, exist_ok=True)
        path = RESULTS / f"{self.name}.json"
        path.write_text(json.dumps(self.data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"evidence: {path} verdict={verdict}")
        return path


def tool_facts():
    out = {}
    for name, path in (("umoci", UMOCI), ("runc", RUNC)):
        if path.exists():
            ver = run([path, "--version"])
            out[name] = {"path": str(path), "sha256": sha256_file(path),
                         "version": ver["stdout"].strip().splitlines()[:4]}
        else:
            out[name] = {"error": "missing"}
    return out


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


# ---------------------------------------------------------------- fixtures

LIB_RE = re.compile(r"=>\s+(/\S+)\s+\(0x[0-9a-f]+\)")
LOADER_RE = re.compile(r"^\s*(/\S*ld-linux\S*)\s+\(0x[0-9a-f]+\)")


def copy_into(root: Path, src: Path):
    """Copy the file at `src` (following symlinks) to the LOGICAL `src` path
    inside rootfs. Symlink chains must be preserved by the caller (usr-merge)."""
    src = Path(src)
    dest = root / src.relative_to("/")
    dest.parent.mkdir(parents=True, exist_ok=True)
    if not dest.exists():
        shutil.copy2(src, dest, follow_symlinks=True)
    return dest


def copy_with_libs(root: Path, binary: str):
    copied = set()
    path = shutil.which(binary) or binary
    # binary lands at its logical path (e.g. /usr/bin/id); the ELF interpreter
    # is an absolute path and must exist literally (e.g. /lib64/ld-linux-*).
    copy_into(root, Path(path))
    copied.add(path)
    ldd = run(["ldd", path])
    for line in ldd["stdout"].splitlines():
        for pattern in (LIB_RE, LOADER_RE):
            match = pattern.search(line)
            if match and ".so" in match.group(1):
                lib = match.group(1)
                if lib not in copied and Path(lib).exists():
                    copy_into(root, Path(lib))
                    copied.add(lib)
    return copied


def ensure_usr_merge(root: Path):
    """Ubuntu is usr-merged: /bin /sbin /lib are symlinks into /usr. Recreate
    the links in the fixture rootfs so logical paths keep working."""
    for link, target in (("bin", "usr/bin"), ("sbin", "usr/sbin"), ("lib", "usr/lib")):
        p = root / link
        if not p.exists() and not p.is_symlink():
            os.symlink(target, p)


def copy_tree(root: Path, src: str, strip_pycache=True):
    src_p = Path(src)
    dest = root / src_p.relative_to("/")
    if dest.exists():
        shutil.rmtree(dest)
    def ignore(dir_, names):
        return {"__pycache__"} if strip_pycache else set()
    shutil.copytree(src_p, dest, ignore=ignore, symlinks=False)


BASE_BINS = ["/bin/sh", "/bin/bash", "/usr/bin/id", "/usr/bin/env", "/bin/pwd",
             "/bin/ls", "/bin/cat", "/bin/echo", "/bin/cp", "/bin/chmod", "/bin/chown",
             "/bin/mkdir", "/bin/rm", "/bin/ln", "/usr/bin/su", "/bin/true", "/bin/sleep",
             "/usr/bin/stat", "/bin/dmesg"]

EXTRA_LIBS = ["/lib/x86_64-linux-gnu/libnss_files.so.2", "/lib/x86_64-linux-gnu/libnss_compat.so.2"]


def payload_python(root: Path):
    py = Path("/usr/bin/python3").resolve()
    copied = copy_with_libs(root, str(py))
    versioned = Path(f"/usr/lib/python{sys.version_info.major}.{sys.version_info.minor}")
    for cand in sorted(Path("/usr/lib").glob("python3.1*"))[:1]:
        copy_tree(root, str(cand))
    # python3 wrapper name expected by scripts
    wrapper = root / "usr/bin/python3"
    if not wrapper.exists():
        wrapper.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(py.name, wrapper)
    return copied


def payload_gcc(root: Path):
    copy_with_libs(root, "/usr/bin/gcc")
    # compiler stages executed by the driver are separate dynamic binaries;
    # each needs its own library closure (libisl/libmpc/libmpfr/libgmp/...).
    for staged in Path("/usr/lib/gcc").rglob("*"):
        if staged.is_file() and staged.name in ("cc1", "cc1plus", "lto1", "collect2"):
            copy_with_libs(root, str(staged))
    for extra in ("/usr/bin/as", "/usr/bin/ld"):
        if Path(extra).exists():
            copy_with_libs(root, extra)
    for tree in ("/usr/lib/gcc",):
        if Path(tree).is_dir():
            copy_tree(root, tree)
    for hdr in ("/usr/include", "/usr/lib/gcc/x86_64-linux-gnu/11/include"):
        if Path(hdr).is_dir():
            copy_tree(root, hdr)
    for crt_pat in ("crt*.o", "Scrt*.o", "Mcrt*.o", "gcrt*.o"):
        for crt in Path("/usr/lib/x86_64-linux-gnu").glob(crt_pat):
            copy_into(root, crt)
    for dev in ("libc.so", "libm.so", "libc_nonshared.a", "libc.a"):
        p = Path("/usr/lib/x86_64-linux-gnu") / dev
        if p.exists():
            copy_into(root, p)


def payload_git(root: Path):
    copy_with_libs(root, "/usr/bin/git")
    core = Path("/usr/lib/git-core")
    if core.is_dir():
        copy_tree(root, str(core))
        for helper in ("git-remote-http", "git-remote-https", "git-upload-pack"):
            helper_path = core / helper
            if helper_path.exists():
                copy_with_libs(root, str(helper_path))


def payload_node(root: Path):
    node = shutil.which("node")
    if node:
        copied = copy_with_libs(root, node)
        # place node at the sandbox PATH location with its content, not a link
        rootbin = root / "usr/bin"
        src = root / Path(node).resolve().relative_to("/")
        if src.exists() and (rootbin / "node") != src:
            shutil.copy2(src, rootbin / "node") if not (rootbin / "node").exists() else None


def fixture_rootfs_a(dest: Path):
    """Fixture A: single-user (root) benchmark-style workload rootfs."""
    dest.mkdir(parents=True, exist_ok=True)
    for binary in BASE_BINS:
        if Path(binary).exists():
            copy_with_libs(dest, binary)
    payload_python(dest)
    payload_gcc(dest)
    payload_git(dest)
    payload_node(dest)
    for lib in EXTRA_LIBS:
        if Path(lib).exists():
            copy_into(dest, Path(lib))
    ensure_usr_merge(dest)
    etc = dest / "etc"
    etc.mkdir(exist_ok=True)
    (etc / "passwd").write_text("root:x:0:0:root:/root:/bin/sh\n")
    (etc / "group").write_text("root:x:0:\n")
    (etc / "os-release").write_text('NAME="LightSandbox Fixture A"\nID=lsbx-fixture-a\nVERSION="1"\n')
    (etc / "resolv.conf").write_text("# fixture placeholder\n")
    for d in ("root", "tmp", "home", "workspace"):
        (dest / d).mkdir(exist_ok=True)
    (dest / "tmp").chmod(0o1777)
    return dest


def fixture_rootfs_b(dest: Path):
    """Fixture B: multi-UID/GID rootfs with image USER 1000:1000."""
    dest.mkdir(parents=True, exist_ok=True)
    for binary in BASE_BINS:
        if Path(binary).exists():
            copy_with_libs(dest, binary)
    for lib in EXTRA_LIBS:
        if Path(lib).exists():
            copy_into(dest, Path(lib))
    ensure_usr_merge(dest)
    etc = dest / "etc"
    etc.mkdir(exist_ok=True)
    (etc / "passwd").write_text(
        "root:x:0:0:root:/root:/bin/sh\n"
        "daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n"
        "app:x:1000:1000:app user:/home/app:/bin/sh\n"
        "svc:x:1001:1001:service user:/srv/svc:/bin/sh\n")
    (etc / "group").write_text(
        "root:x:0:\ndaemon:x:1:\napp:x:1000:\nsvc:x:1001:\n")
    (etc / "shadow").write_text("root:*:19000:0:99999:7:::\napp:!:19000:0:99999:7:::\nsvc:!:19000:0:99999:7:::\n")
    (etc / "os-release").write_text('NAME="LightSandbox Fixture B"\nID=lsbx-fixture-b\nVERSION="1"\n')
    home_app = dest / "home" / "app"
    home_app.mkdir(parents=True, exist_ok=True)
    (home_app / "app.txt").write_text("owned by app(1000)\n")
    srv = dest / "srv" / "svc"
    srv.mkdir(parents=True, exist_ok=True)
    (srv / "svc.txt").write_text("owned by svc(1001)\n")
    os.chown(dest / "home" / "app", 1000, 1000)
    os.chown(dest / "home" / "app" / "app.txt", 1000, 1000)
    os.chmod(dest / "home" / "app", 0o750)
    os.chown(dest / "srv" / "svc", 1001, 1001)
    os.chown(dest / "srv" / "svc" / "svc.txt", 1001, 1001)
    os.chmod(dest / "srv" / "svc", 0o700)
    setuid_id = dest / "usr/bin/whoami-setuid"
    shutil.copy2("/usr/bin/id", setuid_id)
    os.chmod(setuid_id, 0o4755)
    tmp = dest / "tmp"; tmp.mkdir(exist_ok=True); tmp.chmod(0o1777)
    # device node: mknod is allowed with CAP_MKNOD even in this layer
    try:
        dev_null = dest / "dev" / "null"
        dev_null.parent.mkdir(exist_ok=True)
        if dev_null.exists() or dev_null.is_fifo():
            dev_null.unlink()
        os.mknod(dev_null, stat.S_IFCHR | 0o666, os.makedev(1, 3))
        device_note = "created char 1:3"
    except PermissionError:
        device_note = "mknod EPERM at fixture build time"
    for d in ("workspace",):
        (dest / d).mkdir(exist_ok=True)
    return {"device_note": device_note}


def cmd_fixtures(ev: Evidence):
    rootfs_dir = STATE / "rootfs"
    oci = STATE / "fixtures.oci"
    shutil.rmtree(rootfs_dir, ignore_errors=True)
    if oci.exists():
        shutil.rmtree(oci)
    rootfs_dir.mkdir(parents=True)

    # This umoci (1.0-era main branch) requires an explicit layout first;
    # `umoci new` only validates existing ones.
    run_forcing([UMOCI, "init", "--layout", str(oci)], "umoci layout init")

    a = fixture_rootfs_a(rootfs_dir / "a")
    size_a = run(["du", "-sb", str(a)])["stdout"].split()[0]
    run_forcing([UMOCI, "new", "--image", f"{oci}:a"], "umoci new a")
    run_forcing([UMOCI, "insert", "--image", f"{oci}:a", str(a), "/"], "umoci insert a")
    run_forcing([UMOCI, "config", "--image", f"{oci}:a", "--config.cmd", "/bin/sh",
                 "--config.workingdir", "/workspace",
                 "--config.env", "LSBX_FIXTURE=a", "--config.env", "PATH=/usr/bin:/bin",
                 "--config.user", "0:0"], "umoci config a")
    ev.step("fixture A rootfs+built", {"size_bytes": size_a},
            note="single-user root, python3/gcc/git/node payload")

    b_notes = fixture_rootfs_b(rootfs_dir / "b")
    size_b = run(["du", "-sb", str(rootfs_dir / "b")])["stdout"].split()[0]
    run_forcing([UMOCI, "new", "--image", f"{oci}:b"], "umoci new b")
    run_forcing([UMOCI, "insert", "--image", f"{oci}:b", str(rootfs_dir / "b"), "/"], "umoci insert b")
    run_forcing([UMOCI, "config", "--image", f"{oci}:b", "--config.cmd", "/bin/sh",
                 "--config.workingdir", "/workspace",
                 "--config.env", "LSBX_FIXTURE=b", "--config.env", "PATH=/usr/bin:/bin",
                 "--config.user", "1000:1000"], "umoci config b")
    ev.step("fixture B rootfs+built", {"size_bytes": size_b, **b_notes},
            note="multi-uid: root/app(1000)/svc(1001), setuid binary, device node, USER 1000:1000")

    stat_res = run_forcing([UMOCI, "stat", "--image", f"{oci}:a", "--json"], "umoci stat a")
    manifest = json.loads(stat_res["stdout"])
    digest = manifest.get("history", [{}])[0].get("layer", {}).get("digest") or \
        manifest.get("manifest", {}).get("layers", [{}])[-1].get("digest")
    ev.step("image facts", {"stat_a": manifest})
    index = json.loads((oci / "index.json").read_text())
    ev.step("image index", {"index": index, "layer_digest": digest})
    ev.finish("fixtures A/B built from scratch with umoci (no docker daemon involved)")


# ---------------------------------------------------------------- unpack

def ownership_histogram(root: Path):
    hist = {}
    setuid = []
    devices = []
    xattrs = 0
    for dirpath, dirnames, filenames in os.walk(root):
        for name in dirnames + filenames:
            p = Path(dirpath) / name
            st = p.lstat()
            key = f"{st.st_uid}:{st.st_gid}"
            hist[key] = hist.get(key, 0) + 1
            if st.st_mode & 0o4000:
                setuid.append((str(p.relative_to(root)), oct(st.st_mode & 0o7777), f"{st.st_uid}:{st.st_gid}"))
            if stat.S_ISCHR(st.st_mode) or stat.S_ISBLK(st.st_mode):
                devices.append((str(p.relative_to(root)), os.major(st.st_rdev), os.minor(st.st_rdev)))
            try:
                if os.listxattr(p):
                    xattrs += 1
            except OSError:
                pass
    return {"owners": hist, "setuid": setuid, "devices": devices, "entries_with_xattrs": xattrs}


def cmd_unpack(ev: Evidence):
    oci = STATE / "fixtures.oci"
    for tag in ("a", "b"):
        dest = STATE / "bundles" / tag
        shutil.rmtree(dest, ignore_errors=True)
        t0 = time.monotonic()
        res = run([UMOCI, "unpack", "--rootless", "--image", f"{oci}:{tag}", str(dest)])
        ms = int((time.monotonic() - t0) * 1000)
        ev.step(f"umoci unpack --rootless image:{tag}", res, note=f"{ms}ms wall")
        if res["exit"] != 0:
            ev.finish(f"unpack of fixture {tag} FAILED — rootless unpack not viable as-is",)
            return
        hist = ownership_histogram(dest / "rootfs")
        ev.step(f"rootfs metadata measurement ({tag})", hist,
                note="ownership/setuid/devices/xattrs after rootless unpack")
        config = json.loads((dest / "config.json").read_text())
        keep = {k: config.get(k) for k in ("ociVersion", "process", "root", "linux")}
        ev.step(f"generated config skeleton ({tag})", keep)
    ev.finish("umoci unpack --rootless measured for fixtures A/B")


# ---------------------------------------------------------------- bundle/run

MINIMAL_CAPS = {
    "bounding": ["CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_FSETID",
                 "CAP_KILL", "CAP_MKNOD", "CAP_SETGID", "CAP_SETUID",
                 "CAP_SYS_CHROOT", "CAP_SETPCAP"],
    "effective": ["CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_FSETID",
                  "CAP_KILL", "CAP_MKNOD", "CAP_SETGID", "CAP_SETUID",
                  "CAP_SYS_CHROOT", "CAP_SETPCAP"],
    "permitted": ["CAP_CHOWN", "CAP_DAC_OVERRIDE", "CAP_FOWNER", "CAP_FSETID",
                  "CAP_KILL", "CAP_MKNOD", "CAP_SETGID", "CAP_SETUID",
                  "CAP_SYS_CHROOT", "CAP_SETPCAP"],
    "ambient": [],
}


def minimal_config(args, uid=0, gid=0, host_uid=0, host_gid=0, with_proc=False,
                   bind_proc_ro=False, hostname="lsbx", extra_env=None,
                   sandbox_sys_admin=False):
    mounts = [{"destination": "/dev", "type": "tmpfs", "source": "tmpfs",
               "options": ["nosuid", "strictatime", "mode=755", "size=65536k"]}]
    if with_proc:
        mounts.append({"destination": "/proc", "type": "proc", "source": "proc"})
    if bind_proc_ro:
        mounts.append({"destination": "/proc", "type": "bind", "source": "/proc",
                       "options": ["rbind", "ro", "nosuid", "noexec", "nodev"]})
    env = ["PATH=/usr/local/bin:/usr/bin:/bin", "LSBX_SPIKE=1"]
    if extra_env:
        env += extra_env
    caps = {k: list(v) for k, v in MINIMAL_CAPS.items()}
    if sandbox_sys_admin:
        # userns-scoped only: grants power over the sandbox's own namespaces,
        # never the host (same model as bubblewrap's internal setup phase).
        # Used ONLY for the TMPFS_COW mechanism probe, not for default runs.
        for key in ("bounding", "effective", "permitted"):
            caps[key].append("CAP_SYS_ADMIN")
    return {
        "ociVersion": "1.3.0",
        "process": {
            "terminal": False,
            "user": {"uid": uid, "gid": gid},
            "args": args,
            "env": env,
            "cwd": "/workspace",
            "capabilities": caps,
            "noNewPrivileges": True,
            "rlimits": [{"type": "RLIMIT_NOFILE", "hard": 1024, "soft": 1024}],
        },
        "root": {"path": "rootfs", "readonly": False},
        "hostname": hostname,
        "mounts": mounts,
        "linux": {
            "uidMappings": [{"containerID": 0, "hostID": host_uid, "size": 1}],
            "gidMappings": [{"containerID": 0, "hostID": host_gid, "size": 1}],
            "namespaces": [{"type": t} for t in
                           ("user", "pid", "mount", "ipc", "uts", "network")],
        },
    }


def chown_tree(path: Path, uid: int, gid: int):
    for dirpath, dirnames, filenames in os.walk(path):
        for name in dirnames + filenames:
            try:
                os.lchown(Path(dirpath) / name, uid, gid)
            except OSError:
                pass
    try:
        os.chown(path, uid, gid)
    except OSError:
        pass


def cmd_firstrun(ev: Evidence):
    bundle = STATE / "bundles" / "a"
    if not (bundle / "rootfs").is_dir():
        raise SystemExit("run `unpack` first")
    original = (bundle / "config.json").read_text()

    # -- step 0: runc as euid-0 in this nested layer takes the rootful
    #    cgroupfs path and dies on the read-only cgroupfs. Deliberate
    #    baseline measurement; production path runs as the unprivileged user.
    cfg = minimal_config(["/bin/true"])
    (bundle / "config.json").write_text(json.dumps(cfg, indent=1))
    rootful = run([RUNC, "run", "--bundle", str(bundle), "rootful-euid0"])
    ev.step("baseline: runc run as euid-0 (rootful path)", rootful,
            note="documents why the PORTABLE_ROOTLESS runtime must run as the real user")
    run([RUNC, "delete", "--force", "rootful-euid0"])

    # -- switch to the PORTABLE_ROOTLESS shape: runc as uid 65534 -----------
    prepare_nobody_context()
    chown_tree(bundle, NOBODY_UID, NOBODY_GID)

    # -- P0 probe A: fresh private /proc requested --------------------------
    cfg = minimal_config(["/bin/true"], host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                         with_proc=True)
    (bundle / "config.json").write_text(json.dumps(cfg, indent=1))
    p0 = run_as(NOBODY_UID, NOBODY_GID, [RUNC, "run", "--bundle", str(bundle), "p0-proc"])
    ev.step("P0: runc run with fresh private /proc", p0,
            note="expected EPERM layer identification, not assumed")
    run_as(NOBODY_UID, NOBODY_GID, [RUNC, "delete", "--force", "p0-proc"])

    p0_diag = run_as(NOBODY_UID, NOBODY_GID,
                     [RUNC, "run", "--bundle", str(bundle), "--log", str(STATE / "p0.log"),
                      "--log-format", "json", "p0-proc2"])
    if (STATE / "p0.log").exists():
        ev.step("P0 runc debug log",
                {"log_tail": (STATE / "p0.log").read_text(errors="replace")[-3000:]},
                note=str(p0_diag["exit"]))
    run_as(NOBODY_UID, NOBODY_GID, [RUNC, "delete", "--force", "p0-proc2"])

    # -- P0 documented finding (runc>=1.4 hard procfs handle check): without
    #    any /proc in the rootfs, container init fails at ProcSelfOpen with
    #    "unsafe procfs detected" (rootfs /proc dir is overlayfs-typed).
    #    Only remaining shapes: fresh proc mount (EPERM here) or a bind of
    #    the host /proc. Measure both.
    for name, args in (("true", ["/bin/true"]),
                       ("id", ["/usr/bin/id"]),
                       ("pwdcwd", ["/bin/pwd"]),
                       ("env", ["/usr/bin/env"])):
        cfg = minimal_config(args, host_uid=NOBODY_UID, host_gid=NOBODY_GID)
        (bundle / "config.json").write_text(json.dumps(cfg, indent=1))
        res = run_as(NOBODY_UID, NOBODY_GID,
                     [RUNC, "run", "--bundle", str(bundle), f"noproc-{name}"])
        ev.step(f"no-proc shape rejected by runc ({name})", res,
                note="runc>=1.4 procfs handle safety check; rootfs /proc is overlayfs-typed")
        run_as(NOBODY_UID, NOBODY_GID, [RUNC, "delete", "--force", f"noproc-{name}"])

    for name, args in (("true", ["/bin/true"]),
                       ("id", ["/usr/bin/id"]),
                       ("pwdcwd", ["/bin/pwd"]),
                       ("env", ["/usr/bin/env"])):
        cfg = minimal_config(args, host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                             bind_proc_ro=True)
        (bundle / "config.json").write_text(json.dumps(cfg, indent=1))
        res = run_as(NOBODY_UID, NOBODY_GID,
                     [RUNC, "run", "--bundle", str(bundle), f"bindproc-{name}"])
        ev.step(f"first execution with rbind-ro host /proc: {name}", res,
                note="rbind+ro+nosuid+noexec+nodev host /proc (security experiment shape)")
        run_as(NOBODY_UID, NOBODY_GID, [RUNC, "delete", "--force", f"bindproc-{name}"])

    (bundle / "config.json").write_text(original)
    chown_tree(bundle, 0, 0)
    ev.finish("first rootless runc executions recorded (P0 fresh proc, runc proc check, bind-proc shape)")


# ---------------------------------------------------------------- p1/p2

def bundle_run(ev, bundle, name, cfg, as_nobody=True, keep=False, runc=RUNC):
    (Path(bundle) / "config.json").write_text(json.dumps(cfg, indent=1))
    argv = [runc, "run", "--bundle", str(bundle), name]
    res = run_as(NOBODY_UID, NOBODY_GID, argv) if as_nobody else run(argv)
    ev.step(f"container {name}: {' '.join(cfg['process']['args'][:2])[:80]}", res)
    cleanup = [RUNC, "delete", "--force", name]
    if as_nobody:
        run_as(NOBODY_UID, NOBODY_GID, cleanup)
    else:
        run(cleanup)
    return res


def cmd_p1(ev: Evidence):
    """Real programs in the working rootless shape (rbind-ro host /proc,
    dedicated PID ns). runc>=1.4 makes the no-/proc shape impossible, so the
    P1 question becomes: with bound host /proc + own PID ns, what works."""
    bundle = STATE / "bundles" / "a"
    prepare_nobody_context()
    chown_tree(bundle, NOBODY_UID, NOBODY_GID)
    hello = Path("/tmp/lsbx-hello.c")
    hello.write_text('#include <stdio.h>\nint main(){puts("hello-from-container");return 0;}\n')
    cases = [
        ("bash", ["/bin/bash", "-c", "echo bash-ok; echo $((21*2))"]),
        ("python", ["/usr/bin/python3", "-c", "import os,sys;print('py',sys.version.split()[0],'pid',os.getpid())"]),
        ("python-proc-self", ["/usr/bin/python3", "-c",
            "import os;print('fds',len(os.listdir('/proc/self/fd')),'self->',os.readlink('/proc/self'))"]),
        ("python-ps", ["/usr/bin/python3", "-c",
            "import os;pids=[d for d in os.listdir('/proc') if d.isdigit()];print('visible host pids:',len(pids))"]),
        ("git", ["/usr/bin/git", "--version"]),
        ("node", ["/usr/bin/node", "--version"]),
    ]
    for name, args in cases:
        cfg = minimal_config(args, host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                             bind_proc_ro=True)
        bundle_run(ev, bundle, f"p1-{name}", cfg)
    # gcc compile+execute inside the container
    res = bundle_run(ev, bundle, "p1-gcc-prepare", minimal_config(
        ["/bin/cp", "/dev/stdin", "/tmp/hello.c"], host_uid=NOBODY_UID,
        host_gid=NOBODY_GID, bind_proc_ro=True))
    gcc_cfg = minimal_config(["/bin/bash", "-c",
        "printf '#include <stdio.h>\\nint main(){puts(\"hello-from-container\");return 0;}\\n' > /tmp/h.c "
        "&& gcc -O2 -o /tmp/h /tmp/h.c && /tmp/h"],
        host_uid=NOBODY_UID, host_gid=NOBODY_GID, bind_proc_ro=True)
    bundle_run(ev, bundle, "p1-gcc", gcc_cfg)
    chown_tree(bundle, 0, 0)
    ev.finish("P1 measured: real workloads under bound-/proc + dedicated pidns")


# ---------------------------------------------------------------- p2 / uids

P2_PROBE = r'''
import os, sys, ctypes, signal
host = sys.argv[1]
out = {}
seen = []
for p in os.listdir('/proc'):
    if not p.isdigit():
        continue
    try:
        cl = open(f'/proc/{p}/cmdline','rb').read().replace(b'\x00', b' ').strip()
    except OSError:
        continue
    # strict argv[0] match: a substring match self-matches this probe's own
    # -c source text (which literally contains "sleep")
    argv0 = cl.split(b' ', 1)[0]
    if argv0 in (b'sleep', b'/bin/sleep'):
        seen.append((p, cl[:60]))
out['sleep_procs_visible'] = seen
for probe, fn in (
    ('environ', lambda: open(f'/proc/{host}/environ','rb').read().replace(b'\x00', b'|').decode()),
    ('fd_count', lambda: len(os.listdir(f'/proc/{host}/fd'))),
    ('cmdline', lambda: open(f'/proc/{host}/cmdline','rb').read().replace(b'\x00', b' ').decode()),
):
    try:
        out[probe] = fn()
    except OSError as e:
        out[probe] = f'{type(e).__name__}: {e.strerror}'
try:
    os.kill(int(host), 0); out['signal_0_exists'] = 'process visible to kill()'
except OSError as e:
    out['signal_0_exists'] = f'{type(e).__name__}: {e.strerror}'
try:
    os.kill(int(host), signal.SIGTERM); out['sigterm'] = 'SENT (interference possible)'
except OSError as e:
    out['sigterm'] = f'{type(e).__name__}: {e.strerror}'
libc = ctypes.CDLL(None, use_errno=True)
rc = libc.ptrace(16, int(host), 0, 0)  # PTRACE_ATTACH
out['ptrace_attach'] = f'rc={rc} errno={ctypes.get_errno()}'
import json
print(json.dumps(out, ensure_ascii=False, default=str, indent=1))
'''


def start_detached(ev, bundle, name, args, extra_env=None, bind_proc_ro=True):
    """Detached `runc run -d` as the unprivileged user. The container's
    payload INHERITS our stdio, so capture_output pipes would block until the
    payload exits; stdio must go to a log file instead."""
    cfg = minimal_config(args, host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                         bind_proc_ro=bind_proc_ro, hostname=name,
                         extra_env=extra_env)
    (Path(bundle) / "config.json").write_text(json.dumps(cfg, indent=1))
    pid_file = LSBX_RT / f"{name}.pid"
    if pid_file.exists():
        pid_file.unlink()

    def drop():
        os.setgid(NOBODY_GID)
        os.setuid(NOBODY_UID)
        os.environ.pop("HOME", None)
    env = {"HOME": str(LSBX_NOBODY_HOME), "XDG_RUNTIME_DIR": str(LSBX_RT),
           "PATH": "/usr/local/bin:/usr/bin:/bin"}
    argv = [RUNC, "run", "-d", "--pid-file", str(pid_file),
            "--bundle", str(bundle), name]
    log_path = STATE / f"{name}.detached.log"
    t0 = time.monotonic()
    with open(log_path, "w") as log:
        proc = subprocess.run([str(a) for a in argv], stdout=log,
                              stderr=subprocess.STDOUT, env=env,
                              preexec_fn=drop, timeout=120)
    ms = int((time.monotonic() - t0) * 1000)
    res = {"argv": [str(a) for a in argv], "exit": proc.returncode, "ms": ms,
           "run_as": f"{NOBODY_UID}:{NOBODY_GID}",
           "stdout": log_path.read_text(errors="replace")[-2000:], "stderr": ""}
    ev.step(f"detached start {name}", res)
    time.sleep(1.0)
    pid = int(pid_file.read_text().strip()) if pid_file.exists() else None
    return pid, res


def stop_detached(name):
    run_as(NOBODY_UID, NOBODY_GID, [RUNC, "kill", name, "KILL"])
    run_as(NOBODY_UID, NOBODY_GID, [RUNC, "delete", "--force", name])


def cmd_p2(ev: Evidence):
    """SECURITY EXPERIMENT (never a default implementation): sandboxes A and B
    with separate user/pid/mount/ns but the same bound host /proc. Measure
    metadata, signal and ptrace exposure from B to A."""
    bundle_a = STATE / "bundles" / "a"
    bundle_b = STATE / "bundles" / "a2"
    if not bundle_b.exists():
        shutil.copytree(bundle_a, bundle_b, symlinks=True)
    for b in (bundle_a, bundle_b):
        chown_tree(b, NOBODY_UID, NOBODY_GID)
    prepare_nobody_context()
    secret = "SBXSECRET-9f41c2"
    pid_a, start_res = start_detached(ev, bundle_a, "sbx-a",
                                      ["/bin/sleep", "600"],
                                      extra_env=[f"LSBX_SECRET_A={secret}"])
    if pid_a is None:
        ev.finish("P2 aborted: sandbox A failed to start")
        return
    probe = P2_PROBE.replace("sys.argv[1]", repr(str(pid_a)))
    cfg_b = minimal_config(["/usr/bin/python3", "-c", probe],
                           host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                           bind_proc_ro=True, hostname="sbx-b")
    res_b = bundle_run(ev, bundle_b, "sbx-b", cfg_b)
    # is A still alive after B's SIGTERM attempt?
    alive = run_as(NOBODY_UID, NOBODY_GID, [RUNC, "state", "sbx-a"])
    ev.step("sandbox A state after B probe", alive,
            note="pid ns should make SIGTERM undeliverable")
    state_txt = alive["stdout"]
    leaked = secret in res_b["stdout"]
    ev.step("cross-sandbox exposure classification", {
        "a_host_pid": pid_a,
        "secret_in_environ_read_by_b": leaked,
        "b_stdout": res_b["stdout"][:2000],
    }, note="determines whether shared bound /proc is REJECTED as default")
    stop_detached("sbx-a")
    ev.data["sampling"] = ("single-sample security probe: every P2 check ran "
                           "exactly once on this host profile; absence-of-leak "
                           "verdicts are N=1 observations, not statistical proof")
    ev.finish("P2 measured; classification recorded "
              f"(secret leaked via shared /proc: {leaked})")


B_UID_CASES = [
    ("id-root", ["/usr/bin/id"], 0, 0,
     "identity in container as image root"),
    ("ls-ownership", ["/bin/ls", "-la", "/home", "/srv"], 0, 0,
     "ownership after rootless unpack (flattened to 0:0 per measurement)"),
    ("cat-app-file", ["/bin/cat", "/home/app/app.txt"], 0, 0,
     "read file originally owned by uid 1000, mode 0750"),
    ("cat-svc-file", ["/bin/cat", "/srv/svc/svc.txt"], 0, 0,
     "read file originally owned by uid 1001, mode 0700"),
    ("chmod", ["/bin/chmod", "600", "/home/app/app.txt"], 0, 0, "chmod on owned file"),
    ("chown-to-1000", ["/bin/chown", "1000", "/home/app/app.txt"], 0, 0,
     "chown to an unmapped uid (expected kernel EPERM)"),
    ("su-app", ["/usr/bin/su", "-s", "/bin/sh", "app", "-c", "/usr/bin/id"], 0, 0,
     "su to image user (expected BROKEN: unmapped uid / PAM)"),
    ("setuid-binary", ["/usr/bin/whoami-setuid", "-u"], 0, 0,
     "setuid-bit binary after flatten (root==same mapped uid)"),
    ("image-USER-1000", ["/usr/bin/id"], 1000, 1000,
     "run as image USER 1000:1000 (expected runc rejection: uid unmapped)"),
]


def cmd_uids(ev: Evidence):
    """single-ID host multi-user semantics with fixture B (root/app/svc users,
    setuid bit, per-user dirs, image USER 1000:1000)."""
    bundle = STATE / "bundles" / "b"
    if not (bundle / "rootfs").is_dir():
        raise SystemExit("run unpack first")
    prepare_nobody_context()
    chown_tree(bundle, NOBODY_UID, NOBODY_GID)
    classified = {}
    for name, args, uid, gid, note in B_UID_CASES:
        cfg = minimal_config(args, uid=uid, gid=gid,
                             host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                             bind_proc_ro=True)
        res = bundle_run(ev, bundle, f"uid-{name}", cfg)
        ok = res["exit"] == 0
        if name in ("chown-to-1000", "su-app", "image-USER-1000"):
            classified[name] = "SUPPORTED" if ok else "BROKEN"
        elif name == "setuid-binary":
            classified[name] = "EMULATED" if ok else "BROKEN"
        else:
            classified[name] = "SUPPORTED" if ok else "BROKEN"
        classified[name] += f" (exit={res['exit']})"
    ev.step("single-ID multi-user classification", classified,
            note="no silent USER->0 rewriting: failures remain BROKEN evidence")
    chown_tree(bundle, 0, 0)
    ev.finish("single-ID multi-user semantics classified (tests/harness policy: no flatten-and-claim)")



# ---------------------------------------------------------------- rootfs

MNT_TMPFS_PY = ("import ctypes\nlibc=ctypes.CDLL(None,use_errno=True)\n"
                "rc=libc.mount(b'tmpfs',b'/tmp',b'tmpfs',0,b'size=32m')\n"
                "print('mount rc',rc,'errno',ctypes.get_errno(),flush=True)\n"
                "assert rc==0\n"
                "open('/tmp/scratch','wb').write(b'0'*8388608)\nprint('scratch-ok 8MB',flush=True)\n"
                "rc=libc.umount2(b'/tmp',0)\nprint('umount rc',rc,'errno',ctypes.get_errno())\n")

MNT_LAZY_PY = ("import ctypes,time\nlibc=ctypes.CDLL(None,use_errno=True)\n"
               "rc=libc.mount(b'tmpfs',b'/tmp',b'tmpfs',0,None)\nassert rc==0,ctypes.get_errno()\n"
               "t0=time.monotonic()\ndata=open('/usr/bin/python3.10','rb').read()\n"
               "open('/tmp/p','xb').write(data)\n"
               "print('lazy copy-on-first-write ms',int((time.monotonic()-t0)*1000),'bytes',len(data))\n"
               "libc.umount2(b'/tmp',0)\n")

MNT_MASK_PY = ("import ctypes\nlibc=ctypes.CDLL(None,use_errno=True)\n"
               "rc=libc.mount(b'/etc',b'/etc',None,4096,None)\nassert rc==0,('bind',ctypes.get_errno())\n"
               "rc=libc.mount(None,b'/etc',None,4096|32|1,None)\nassert rc==0,('remount',ctypes.get_errno())\n"
               "try:\n open('/etc/hostname','w');print('write-UNEXPECTED-ok')\n"
               "except OSError as e: print('write-blocked errno',e.errno)\n"
               "rc=libc.umount2(b'/etc',0)\nprint('mask-umount rc',rc)\n")

MNT_MEM_PY = ("import ctypes,re\nlibc=ctypes.CDLL(None,use_errno=True)\n"
              "avail=lambda:int(re.search(r'MemAvailable:\\s+(\\d+)',open('/proc/meminfo').read()).group(1))\n"
              "b0=avail()\nrc=libc.mount(b'tmpfs',b'/tmp',b'tmpfs',0,b'size=64m')\nassert rc==0,ctypes.get_errno()\n"
              "open('/tmp/scratch','wb').write(b'0'*33554432)\nb1=avail()\n"
              "print('mem_available_delta_kb_while_live',b0-b1)\n")


def cmd_rootfs(ev: Evidence):
    """Only the two sanctioned rootfs strategies for PORTABLE_ROOTLESS:
    FULL_COPY (persistent, disk-backed) and TMPFS_COW (ephemeral, memory-bound,
    mechanism level). No disk-overlay path (host kernel EPERM), no tuning."""
    base = STATE / "bundles" / "a"
    if not (base / "rootfs").is_dir():
        raise SystemExit("run unpack first")
    prepare_nobody_context()
    chown_tree(base, NOBODY_UID, NOBODY_GID)

    # ---- FULL_COPY: prepare -> write -> restart-read (persistence) -> delete
    dst = STATE / "rootfs-fullcopy"
    shutil.rmtree(dst, ignore_errors=True)
    os.makedirs(dst, exist_ok=True)
    # the destination top dir must be owned by the sandbox uid, otherwise the
    # delete step (performed as that uid) cannot remove entries inside it
    os.chown(dst, NOBODY_UID, NOBODY_GID)
    t0 = time.monotonic()
    cp = run(["/bin/cp", "-a", str(base) + "/.", str(dst)])
    prep_ms = int((time.monotonic() - t0) * 1000)
    ev.step("FULL_COPY prepare (cp -a)", cp, note=f"{prep_ms} ms wall")
    ev.step("FULL_COPY du", run(["/usr/bin/du", "-sb",
                                 str(base / "rootfs"), str(dst / "rootfs")]))
    w = bundle_run(ev, dst, "fc-write", minimal_config(
        ["/bin/bash", "-c", "echo marker-$RANDOM > /workspace/marker.txt; cat /workspace/marker.txt"],
        host_uid=NOBODY_UID, host_gid=NOBODY_GID, bind_proc_ro=True))
    r = bundle_run(ev, dst, "fc-reread", minimal_config(
        ["/bin/cat", "/workspace/marker.txt"],
        host_uid=NOBODY_UID, host_gid=NOBODY_GID, bind_proc_ro=True))
    persistence = (r["exit"] == 0 and w["exit"] == 0
                   and r["stdout"].strip() == w["stdout"].strip() != "")
    ev.step("FULL_COPY persistence across two container starts",
            {"written": w["stdout"].strip(), "reread": r["stdout"].strip(),
             "persistent": persistence})
    t0 = time.monotonic()
    dres = run_as(NOBODY_UID, NOBODY_GID, ["/bin/bash", "-c",
                  f"/bin/rm -rf --one-file-system {dst}/rootfs {dst}/config.json"])
    ev.step("FULL_COPY delete (contents as uid 65534; top dir is host state, not sandbox)",
            dres, note=f"{int((time.monotonic() - t0) * 1000)} ms wall")

    # ---- TMPFS_COW: mechanism inside the sandbox via mount(2) directly ------
    # (fixture has no util-linux mount binary; python ctypes invokes the same
    # syscalls the supervisor would issue)
    tmpfs = bundle_run(ev, base, "cow-tmpfs-scratch", minimal_config(
        ["/usr/bin/python3", "-c", MNT_TMPFS_PY],
        host_uid=NOBODY_UID, host_gid=NOBODY_GID, bind_proc_ro=True,
        sandbox_sys_admin=True))
    lazy = bundle_run(ev, base, "cow-lazy-copy-micro", minimal_config(
        ["/usr/bin/python3", "-c", MNT_LAZY_PY],
        host_uid=NOBODY_UID, host_gid=NOBODY_GID, bind_proc_ro=True,
        sandbox_sys_admin=True))
    mask = bundle_run(ev, base, "cow-bind-ro-mask", minimal_config(
        ["/usr/bin/python3", "-c", MNT_MASK_PY],
        host_uid=NOBODY_UID, host_gid=NOBODY_GID, bind_proc_ro=True,
        sandbox_sys_admin=True))
    mem = bundle_run(ev, base, "cow-mem-impact", minimal_config(
        ["/usr/bin/python3", "-c", MNT_MEM_PY],
        host_uid=NOBODY_UID, host_gid=NOBODY_GID, bind_proc_ro=True,
        sandbox_sys_admin=True))
    ev.step("TMPFS memory boundedness (32MB tmpfs file, host MemAvailable delta measured INSIDE the live sandbox)",
            {"delta": [l for l in mem["stdout"].splitlines() if "delta" in l]})
    ev.step("TMPFS_COW architecture block", {
        "why_runc_cannot_wire_it": "a tmpfs COW layer must be mounted inside the sandbox "
            "user+mount namespaces, but runc prepares rootfs mounts before that userns "
            "owns anything; a nested launcher is required",
        "required_block": "per-sandbox supervisor sharing the container mountns: "
            "mount tmpfs -> userspace lazy copy-on-write (no FUSE: /dev/fuse absent)",
        "disk_overlay": "excluded by design AND by host EPERM evidence",
        "v1_shape": "FULL_COPY (persistent) works today; TMPFS_COW = mechanism proven, "
            "runtime wiring pending the supervisor architecture block"})
    mech_ok = (tmpfs["exit"] == 0 and lazy["exit"] == 0
               and "write-blocked" in mask["stdout"])
    ev.finish(f"rootfs: FULL_COPY {'WORKS' if persistence else 'BROKEN'}; "
              f"TMPFS_COW mechanism {'PROVEN' if mech_ok else 'BLOCKED'} (supervisor block required for runtime wiring)")


# ---------------------------------------------------------------- isolation

NET_SRV = ("import socket\ns=socket.socket()\ns.bind(('127.0.0.1',18231))\ns.listen(1)\n"
           "print('LISTEN',flush=True)\nc,_=s.accept()\nc.sendall(b'PONG')\n")
NET_CLI = ("import socket\ns=socket.socket()\ns.settimeout(2)\ntry:\n s.connect(('127.0.0.1',18231))\n"
           " print('CONNECTED')\nexcept OSError as e:\n print('FAIL',type(e).__name__,e.errno)\n")
HOST_PROBE = ("import socket\ns=socket.socket()\ns.settimeout(2)\ntry:\n s.connect(('127.0.0.1',22))\n"
              " print('REACHABLE')\nexcept OSError as e:\n print('FAIL',type(e).__name__,e.errno)\n")
ABS_SRV = ("import socket\ns=socket.socket(socket.AF_UNIX)\ns.bind('\\x00mtxabs')\ns.listen(1)\n"
           "print('LISTEN',flush=True)\nc,_=s.accept()\nc.sendall(b'PONG')\n")
ABS_CLI = ("import socket\ns=socket.socket(socket.AF_UNIX)\ns.settimeout(2)\ntry:\n s.connect('\\x00mtxabs')\n"
           " print('CONNECTED')\nexcept OSError as e:\n print('FAIL',type(e).__name__,e.errno)\n")
SHM_CREATE = ("import ctypes\nlibc=ctypes.CDLL(None,use_errno=True)\nlibc.shmget.restype=ctypes.c_int\n"
              "shmid=libc.shmget(0,64,0o1000|0o600)\nprint('SHMID',shmid,'errno',ctypes.get_errno())\n")
SHM_ATTACH = ("import ctypes\nlibc=ctypes.CDLL(None,use_errno=True)\nlibc.shmat.restype=ctypes.c_void_p\n"
              "ctypes.set_errno(0)\np=libc.shmat(SHMID_TOK,None,0)\ne=ctypes.get_errno()\n"
              "ok = e == 0 and p not in (None, 0, 18446744073709551615)\n"
              "print('ATTACH_OK' if ok else 'ATTACH_FAIL errno', e, p)\n")
PTRACE_CODE = ("import ctypes\ntry:\n rc=ctypes.CDLL(None,use_errno=True).ptrace(16,%d,0,0)\n"
               " print('rc',rc,'errno',ctypes.get_errno())\nexcept Exception as e:\n print('exc',e)\n")


def run_as_bg(argv_s, out_path):
    def drop():
        os.setgid(NOBODY_GID)
        os.setuid(NOBODY_UID)
        os.environ.pop("HOME", None)
    env = {"HOME": str(LSBX_NOBODY_HOME), "XDG_RUNTIME_DIR": str(LSBX_RT),
           "PATH": "/usr/local/bin:/usr/bin:/bin"}
    fh = open(out_path, "w")
    return subprocess.Popen([str(a) for a in argv_s], stdout=fh,
                            stderr=subprocess.STDOUT, env=env, preexec_fn=drop)


def iso_exec(name, args):
    return run_as(NOBODY_UID, NOBODY_GID, [RUNC, "exec", name] + [str(a) for a in args])


def _is_eacces(res):
    """True only when the probe result carries EACCES evidence (a python
    PermissionError traceback or an explicit 'errno 13' print). Any other
    failure must NOT be upgraded to an isolation verdict."""
    blob = res["stdout"] + res["stderr"]
    return "PermissionError" in blob or "Errno 13" in blob or "errno 13" in blob


def cmd_isolation(ev: Evidence):
    """Cross-sandbox matrix for the ONLY currently viable native shape:
    user+pid+mount+ipc+uts+network namespaces, single-ID mapping, rbind-ro host
    /proc. A=iso-a, B=iso-b, both as host uid 65534."""
    a = STATE / "bundles" / "a"
    b = STATE / "bundles" / "a2iso"
    if not (a / "rootfs").is_dir():
        raise SystemExit("run unpack first")
    prepare_nobody_context()
    chown_tree(a, NOBODY_UID, NOBODY_GID)
    shutil.rmtree(b, ignore_errors=True)
    os.makedirs(b, exist_ok=True)
    ev.step("prepare sandbox B bundle (copy of A)",
            run(["/bin/cp", "-a", str(a) + "/.", str(b)]))
    secret = "MTX-" + os.urandom(6).hex()
    pid_a, _ = start_detached(ev, a, "iso-a", ["/bin/sleep", "600"],
                              extra_env=[f"LSBX_SECRET_A={secret}"])
    pid_b, _ = start_detached(ev, b, "iso-b", ["/bin/sleep", "600"],
                              extra_env=["LSBX_SECRET_B=placeholder"])
    matrix = []

    def axis(name, verdict, detail, note=""):
        matrix.append({"axis": name, "classification": verdict,
                       "detail": str(detail)[:400], "note": note})

    try:
        if not pid_a or not pid_b:
            ev.finish("isolation aborted: sandbox failed to start")
            return
        w = iso_exec("iso-b", ["/bin/bash", "-c", "echo bwas > /tmp/b_marker"])
        if w["exit"] != 0:
            # if the B-side write silently failed, A's failing read would
            # falsely confirm isolation — refuse to classify
            axis("filesystem", "NOT_TESTED",
                 f"B-side marker write failed rc={w['exit']}: "
                 f"{(w['stdout'] + w['stderr']).strip()}")
        else:
            r = iso_exec("iso-a", ["/bin/bash", "-c", "cat /tmp/b_marker"])
            axis("filesystem", "ISOLATED" if r["exit"] != 0 else "INTERFERABLE",
                 f"write rc=0; A read: {(r['stdout'] + r['stderr']).strip()}",
                 "separate rootfs copies + own mount ns")

        r = iso_exec("iso-a", ["/usr/bin/python3", "-c",
                     "import os;print(len([d for d in os.listdir('/proc') if d.isdigit()]))"])
        n = int(r["stdout"].strip() or 0)
        axis("process_metadata", "VISIBLE_READONLY", f"{n} host pids visible via bound host /proc",
             "host-level numbering; contents subject to ptrace_may_access of same-uid processes")

        r = iso_exec("iso-b", ["/usr/bin/python3", "-c",
                     f"import os\ntry:\n os.kill({pid_a},0);print('EXISTS')\n"
                     f"except OSError as e:print('errno',e.errno)"])
        axis("signals", "ISOLATED" if "errno 3" in r["stdout"] else "INTERFERABLE",
             r["stdout"].strip(), "kill() resolves in caller pid ns; A's host pid has no mapping in B")

        r = iso_exec("iso-b", ["/usr/bin/python3", "-c", PTRACE_CODE % pid_a])
        ptr_isolated = any(f"errno {e}" in r["stdout"] for e in (1, 3))
        axis("ptrace", "ISOLATED" if ptr_isolated else "INTERFERABLE", r["stdout"].strip(),
             "PTRACE_ATTACH to A's host pid: ESRCH(president ns) or EPERM both block control")

        r = iso_exec("iso-b", ["/usr/bin/python3", "-c",
                     f"print(open('/proc/{pid_a}/cmdline','rb').read()[:200])"])
        # classify on evidence, not on "probe did not print": a crashed probe
        # must be INCONCLUSIVE, never ISOLATED
        axis("proc_cmdline_of_A",
             "VISIBLE" if (r["exit"] == 0 and r["stdout"].strip())
             else "ISOLATED" if _is_eacces(r) else "INCONCLUSIVE",
             (r["stdout"] + r["stderr"]).strip())

        r = iso_exec("iso-b", ["/usr/bin/python3", "-c",
                     f"print(open('/proc/{pid_a}/environ','rb').read()[:300])"])
        leaked = secret.encode().decode() in r["stdout"]
        axis("proc_environ_of_A",
             "VISIBLE" if leaked
             else "ISOLATED" if _is_eacces(r) else "INCONCLUSIVE",
             (r["stdout"] + r["stderr"]).strip(),
             "SECURITY EXPERIMENT result; ISOLATED only on EACCES/errno 13 evidence")

        r = iso_exec("iso-b", ["/usr/bin/python3", "-c",
                     f"import os\ntry:\n print('FD_COUNT',len(os.listdir('/proc/{pid_a}/fd')))\n"
                     f"except OSError as e: print('errno',e.errno)"])
        axis("proc_fd_of_A",
             "VISIBLE" if "FD_COUNT" in r["stdout"]
             else "ISOLATED" if "errno 13" in r["stdout"] else "INCONCLUSIVE",
             (r["stdout"] + r["stderr"]).strip())

        srv = run_as_bg([RUNC, "exec", "iso-b", "/usr/bin/python3", "-c", NET_SRV],
                        STATE / "iso-netsrv.log")
        time.sleep(2.0)
        # positive control: the same client MUST succeed inside B's own netns,
        # otherwise a dead listener would fake an ISOLATED verdict
        ctl = iso_exec("iso-b", ["/usr/bin/python3", "-c", NET_CLI])
        if "CONNECTED" not in ctl["stdout"]:
            srv.terminate()
            axis("network_tcp_loopback", "NOT_TESTED",
                 f"same-netns control failed: "
                 f"{(ctl['stdout'] + ctl['stderr']).strip()}")
        else:
            r = iso_exec("iso-a", ["/usr/bin/python3", "-c", NET_CLI])
            srv.terminate()
            axis("network_tcp_loopback",
                 "ISOLATED" if "FAIL" in r["stdout"] else "INTERFERABLE",
                 f"control CONNECTED; A: {r['stdout'].strip()}",
                 "private netns per sandbox (loopback-only); no cross-sandbox TCP")

        # positive control: only run the sandbox probe if the host actually
        # has a listener on 127.0.0.1:22, else the axis is vacuous
        host_has_sshd = False
        try:
            import socket as _sock
            _s = _sock.socket()
            _s.settimeout(1.5)
            _s.connect(("127.0.0.1", 22))
            host_has_sshd = True
            _s.close()
        except OSError:
            host_has_sshd = False
        if not host_has_sshd:
            axis("network_host_loopback", "NOT_TESTED",
                 "no host listener on 127.0.0.1:22; cross-probe would be vacuous")
        else:
            r = iso_exec("iso-a", ["/usr/bin/python3", "-c", HOST_PROBE])
            axis("network_host_loopback", "ISOLATED" if "FAIL" in r["stdout"] else "REACHABLE",
                 f"host sshd present; A: {r['stdout'].strip()}",
                 "host loopback services unreachable from sandbox netns")

        srv = run_as_bg([RUNC, "exec", "iso-b", "/usr/bin/python3", "-c", ABS_SRV],
                        STATE / "iso-abssrv.log")
        time.sleep(2.0)
        ctl = iso_exec("iso-b", ["/usr/bin/python3", "-c", ABS_CLI])
        if "CONNECTED" not in ctl["stdout"]:
            srv.terminate()
            axis("unix_abstract_socket", "NOT_TESTED",
                 f"same-netns control failed: "
                 f"{(ctl['stdout'] + ctl['stderr']).strip()}")
        else:
            r = iso_exec("iso-a", ["/usr/bin/python3", "-c", ABS_CLI])
            srv.terminate()
            axis("unix_abstract_socket",
                 "ISOLATED" if "FAIL" in r["stdout"] else "INTERFERABLE",
                 f"control CONNECTED; A: {r['stdout'].strip()}",
                 "abstract unix namespace is per-netns")

        r = iso_exec("iso-a", ["/usr/bin/python3", "-c", SHM_CREATE])
        m = re.search(r"SHMID (-?\d+)", r["stdout"])
        shmid = int(m.group(1)) if m else -1
        if shmid >= 0:
            # positive control: attaching within A's own IPC ns MUST succeed,
            # otherwise a broken probe would fake an ISOLATED verdict
            ctl = iso_exec("iso-a", ["/usr/bin/python3", "-c", SHM_ATTACH.replace("SHMID_TOK", str(shmid))])
            if "ATTACH_OK" not in ctl["stdout"]:
                axis("sysv_shm", "NOT_TESTED",
                     f"same-IPC-ns attach control failed: {ctl['stdout'].strip()}")
            else:
                r = iso_exec("iso-b", ["/usr/bin/python3", "-c", SHM_ATTACH.replace("SHMID_TOK", str(shmid))])
                if "ATTACH_OK" in r["stdout"]:
                    cls = "INTERFERABLE"
                elif "ATTACH_FAIL errno 22" in r["stdout"] or "ATTACH_FAIL errno 2" in r["stdout"]:
                    cls = "ISOLATED"
                else:
                    cls = "INCONCLUSIVE"
                axis("sysv_shm", cls,
                     f"A created shmid={shmid}; A-attach ATTACH_OK; B attach: {r['stdout'].strip()}",
                     "own IPC ns per sandbox; A's shmid is invalid in B")
        else:
            axis("sysv_shm", "NOT_TESTED", f"create failed: {r['stdout'].strip()}")

        r = iso_exec("iso-a", ["/usr/bin/python3", "-c", "import socket;print(socket.gethostname())"])
        axis("hostname", "ISOLATED" if r["stdout"].strip() == "iso-a" else "SHARED",
             r["stdout"].strip(), "uts ns per sandbox")

        axis("environment", "ISOLATED-at-exec / VISIBLE-via-/proc",
             "exec envs are per-container; the ONLY cross-read path is the shared bound host /proc (see proc_environ_of_A)")
    finally:
        stop_detached("iso-a")
        stop_detached("iso-b")

    ev.step("cross-sandbox isolation matrix", matrix)
    ev.data["sampling"] = ("single-sample security probes: each axis probe ran "
                           "exactly once (timing steps elsewhere repeat 3-5x); "
                           "classifications are N=1 observations on this host "
                           "profile, with positive controls recorded per axis")
    bad = [m["axis"] for m in matrix if m["classification"] in ("INTERFERABLE", "REACHABLE")]
    exposed = [m["axis"] for m in matrix if m["classification"].startswith("VISIBLE")]
    ev.finish(f"{len(matrix)} axes; interference={bad or 'none'}; metadata-exposed={exposed or 'none'}")


# ---------------------------------------------------------------- p21

P21_PAYLOAD = (
    "import os,time,socket\n"
    "tok=os.environ['LSBX_P21_TOKEN']\n"
    "os.makedirs('/tmp/'+tok+'-cwd',exist_ok=True)\n"
    "os.chdir('/tmp/'+tok+'-cwd')\n"
    "f=open('/tmp/'+tok+'-file','w')\n"
    "s=socket.socket(socket.AF_UNIX)\n"
    "s.bind('/tmp/'+tok+'.sock')\n"
    "print('P21SELFHOSTPID='+os.readlink('/proc/self').rsplit('/',1)[-1],flush=True)\n"
    "time.sleep(300)\n"
)

P21_READER = r'''
import os, sys, json, errno
pid = sys.argv[1]
tok = sys.argv[2]
tb = tok.encode()
def clip(b, n=72):
    return b[:n].replace(tb, b'<TOK>').decode('utf-8', 'replace')
rows = []
def classify(name, path, marker_expected, link=False):
    row = {'path': name, 'marker_expected': marker_expected}
    try:
        if link:
            t = os.readlink(path)
            row['bytes'] = len(t)
            row['marker_found'] = tok in t
            row['sample'] = clip(t.encode())
            row['class'] = 'CONTENT_VISIBLE'
        else:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            try:
                data = os.read(fd, 65536)
            finally:
                os.close(fd)
            row['bytes'] = len(data)
            row['marker_found'] = tb in data
            row['sample'] = clip(data.replace(b'\x00', b'|'))
            row['class'] = 'CONTENT_VISIBLE' if data else 'METADATA_ONLY'
    except OSError as e:
        row['oserror'] = f'errno={e.errno} {e.strerror}'
        row['class'] = 'HIDDEN' if e.errno == errno.ENOENT else 'ACCESS_DENIED'
    rows.append(row)
b = '/proc/' + pid
classify('cmdline', b + '/cmdline', True)
classify('environ', b + '/environ', True)
classify('cwd', b + '/cwd', True, link=True)
classify('root', b + '/root', False, link=True)
row = {'path': 'fd', 'marker_expected': True}
try:
    names = os.listdir(b + '/fd')
    links = []
    for n in sorted(names, key=int)[:16]:
        try:
            t = os.readlink(b + '/fd/' + n)
            links.append({'fd': n, 'target': clip(t.encode()), 'marker': tok in t})
        except OSError as e:
            links.append({'fd': n, 'oserror': 'errno=%s' % e.errno})
    row['fd_count'] = len(names)
    row['links'] = links
    row['marker_found'] = any(l.get('marker') for l in links)
    row['class'] = 'CONTENT_VISIBLE'
except OSError as e:
    row['oserror'] = 'errno=%s %s' % (e.errno, e.strerror)
    row['class'] = 'HIDDEN' if e.errno == errno.ENOENT else 'ACCESS_DENIED'
rows.append(row)
for nm in ('maps', 'status', 'mountinfo', 'io', 'stat', 'wchan', 'syscall', 'stack'):
    classify(nm, b + '/' + nm, False)
print(json.dumps(rows))
'''


def cmd_p21(ev: Evidence):
    """P2.1 /proc confidentiality matrix (SECURITY EXPERIMENT, feasibility-only).
    Sandbox B and a same-uid DUMPABLE CONTROL process carry artificial random
    markers (argv / env / cwd / open-file / unix-socket-path); sandbox A
    (cross-sandbox, same host uid 65534, own userns+pidns, rbind-ro host /proc)
    attempts reads. Classification per path:
    HIDDEN / METADATA_ONLY / CONTENT_VISIBLE / ACCESS_DENIED.
    Markers are random LSBX_SECRET_<hex>; no real secrets are used or touched.
    The control isolates WHAT protects payloads: userns non-dumpable vs uid."""
    a = STATE / "bundles" / "a"
    if not (a / "rootfs").is_dir():
        raise SystemExit("run unpack first")
    b = STATE / "bundles" / "p21b"
    prepare_nobody_context()
    chown_tree(a, NOBODY_UID, NOBODY_GID)
    shutil.rmtree(b, ignore_errors=True)
    os.makedirs(b, exist_ok=True)
    run(["/bin/cp", "-a", str(a) + "/.", str(b)])
    chown_tree(b, NOBODY_UID, NOBODY_GID)
    tok = "LSBX_SECRET_" + os.urandom(8).hex()
    ctrl = None
    try:
        pid_b, _ = start_detached(
            ev, b, "p21-b",
            ["/usr/bin/python3", "-c", P21_PAYLOAD, tok + "-argv"],
            extra_env=[f"LSBX_P21_TOKEN={tok}", f"LSBX_SECRET_ENV={tok}-env"])
        if not pid_b:
            ev.finish("P2.1 aborted: sandbox B failed to start")
            return
        self_pid = ""
        log = STATE / "p21-b.detached.log"
        if log.exists():
            m = re.search(r"P21SELFHOSTPID=(\d+)", log.read_text(errors="replace"))
            self_pid = m.group(1) if m else ""
        ev.step("sandbox B live with markers (pid-file vs payload self-discovery)",
                {"pid_file_host_pid": pid_b,
                 "payload_self_discovered_host_pid": self_pid,
                 "consistent": str(pid_b) == self_pid,
                 "marker_shapes": ["argv_suffix", "env_value", "cwd_path",
                                   "open_file_path", "unix_socket_path"]})
        pid_c = None
        setpriv = shutil.which("setpriv")
        if setpriv:
            ctrl = subprocess.Popen([
                setpriv, "--reuid", str(NOBODY_UID), "--regid", str(NOBODY_GID),
                "--clear-groups", "env", f"LSBX_SECRET_CTRL={tok}-ctrlenv",
                "/bin/bash", "-c", f"exec -a {tok}-ctrlargv sleep 300"],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            pid_c = ctrl.pid
            time.sleep(0.3)

        def dumpable(pid):
            try:
                m = re.search(r"^Dumpable:\s+(\d+)", open(f"/proc/{pid}/status").read(), re.M)
                return int(m.group(1)) if m else None
            except (OSError, ValueError):
                return None
        ev.step("kernel dumpable flags (harness-side read, mechanism evidence)",
                {"sandbox_b_payload_userns": dumpable(pid_b),
                 "control_same_uid_no_userns": dumpable(pid_c)},
                note="userns entry clears dumpable -> same-uid ptrace_may_access fails")

        def read_matrix(target_pid, label):
            cfg = minimal_config(
                ["/usr/bin/python3", "-c", P21_READER, str(target_pid), tok],
                host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                bind_proc_ro=True, hostname="p21a")
            res = bundle_run(ev, a, f"p21-a-{label}", cfg)
            try:
                rows = json.loads(res["stdout"].strip().splitlines()[-1])
            except (ValueError, IndexError, AttributeError):
                rows = []
            return rows

        def summarize(rows):
            return {r["path"]: {k: r.get(k) for k in ("class", "marker_found", "oserror")}
                    for r in rows}

        rows_b = read_matrix(pid_b, "xb")
        ev.step("cross-sandbox proc confidentiality matrix (A reads B payload)",
                summarize(rows_b), extra={"raw_rows": rows_b})
        rows_c = read_matrix(pid_c, "xc") if pid_c else []
        if rows_c:
            ev.step("same-uid dumpable control matrix (A reads control process)",
                    summarize(rows_c), extra={"raw_rows": rows_c},
                    note="if this leaks while B does not, the protection is the "
                         "userns non-dumpable property, not the uid split")

        def leaks(rows):
            return sorted({r["path"] for r in rows
                           if r.get("class") == "CONTENT_VISIBLE" and r.get("marker_found")})
        leaks_b, leaks_c = leaks(rows_b), (leaks(rows_c) if rows_c else None)
        verdict = {
            "token": tok,
            "cross_sandbox_marker_content_visible_paths": leaks_b,
            "control_marker_content_visible_paths": leaks_c,
            "proc_confidentiality": ("CONTENT_LEAKS_CROSS_SANDBOX" if leaks_b
                                     else "CONTENT_HIDDEN_FROM_CROSS_SANDBOX"),
            "metadata_visibility": "host pid enumeration + per-pid dir metadata remain visible",
            "integrity_isolation": "established separately in isolation.json (13 axes)",
            "worker_trust_model": "single-trust-domain workers: hostile multi-tenant "
                                  "confidentiality is NOT claimed for PORTABLE_ROOTLESS",
            "secret_handling_rule": "secrets must not enter argv/env of sandbox payloads "
                                    "until proc confidentiality is contract-guaranteed",
        }
        ev.step("P2.1 verdict", verdict)
        path = ev.finish(f"P2.1: cross-sandbox {verdict['proc_confidentiality']}; "
                         f"control leaks={leaks_c}")
        sec = REPO / "evidence" / "security"
        sec.mkdir(parents=True, exist_ok=True)
        shutil.copy(path, sec / "proc-confidentiality.json")
        print(f"canonical: {sec / 'proc-confidentiality.json'}")
    finally:
        stop_detached("p21-b")
        if ctrl:
            ctrl.terminate()
        shutil.rmtree(b, ignore_errors=True)


# ---------------------------------------------------------------- seccomp

RSECC = TOOLS / "runc-seccomp"

PERS_PY = ("import ctypes\nlibc=ctypes.CDLL(None,use_errno=True)\n"
           "rc=libc.personality(0xffffffff)\n"
           "print('personality rc',rc,'errno',ctypes.get_errno())\n")

SECCOMP_PROFILE = {
    "defaultAction": "SCMP_ACT_ALLOW",
    "architectures": ["SCMP_ARCH_X86_64", "SCMP_ARCH_X86", "SCMP_ARCH_X32"],
    "syscalls": [{"names": ["personality"], "action": "SCMP_ACT_ERRNO"}],
}


def cmd_seccomp(ev: Evidence):
    """seccomp runtime test (feasibility): build from scripts/build-runc-seccomp.sh
    (user-local libseccomp + locked runc source, seccomp tag). Profile =
    allow-all EXCEPT one harmless-but-detectable syscall (personality ->
    SCMP_ACT_ERRNO). Proves (a) the filter is actually enforced, (b) real
    workloads are unaffected by an allow-all baseline. Deliberately NOT a copy
    of the Docker default profile: policy design is T30 territory."""
    bundle = STATE / "bundles" / "a"
    if not (bundle / "rootfs").is_dir():
        raise SystemExit("run unpack first")
    if not RSECC.exists():
        raise SystemExit("run scripts/build-runc-seccomp.sh first")
    prepare_nobody_context()
    chown_tree(bundle, NOBODY_UID, NOBODY_GID)

    def cfg_for(args, with_profile):
        cfg = minimal_config(args, host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                             bind_proc_ro=True)
        if with_profile:
            cfg["linux"]["seccomp"] = SECCOMP_PROFILE
        return cfg

    # (a) enforcement proof: personality allowed without profile, denied with it
    r0 = bundle_run(ev, bundle, "sc-noprofile-personality",
                    cfg_for(["/usr/bin/python3", "-c", PERS_PY], False),
                    runc=RSECC)
    r1 = bundle_run(ev, bundle, "sc-profile-personality",
                    cfg_for(["/usr/bin/python3", "-c", PERS_PY], True),
                    runc=RSECC)
    enforced = ("errno 1" in r1["stdout"]) and (
        "errno 1" not in r0["stdout"]) and (r0["exit"] == 0) and (r1["exit"] == 0)
    ev.step("seccomp enforcement (personality baseline vs denied)", {
        "without_profile": r0["stdout"].strip(),
        "with_profile": r1["stdout"].strip(), "enforced": enforced})

    # (b) allow-all baseline leaves real workloads intact
    workloads = [
        ("bash", ["/bin/bash", "-c", "echo bash-ok; echo $((6*7))"], "bash-ok"),
        ("python3", ["/usr/bin/python3", "-c", "print('py-ok')"], "py-ok"),
        ("git", ["/usr/bin/git", "--version"], "git version"),
        ("node", ["/usr/bin/node", "-e", "console.log('node-ok')"], "node-ok"),
        ("gcc", ["/bin/bash", "-c",
                 "printf 'int main(){return 0;}\\n' > /tmp/s.c "
                 "&& gcc -O2 -o /tmp/s /tmp/s.c && /tmp/s; echo gcc-exit-$?"],
         "gcc-exit-0"),
    ]
    ok, failed = 0, []
    for name, args, marker in workloads:
        r = bundle_run(ev, bundle, f"sc-profile-{name}", cfg_for(args, True),
                       runc=RSECC)
        good = r["exit"] == 0 and marker in r["stdout"]
        ok += 1 if good else 0
        if not good:
            failed.append(name)
    chown_tree(bundle, 0, 0)
    ev.finish(f"seccomp: enforced={enforced}; workloads_ok={ok}/{len(workloads)}"
              f"{'; failed=' + ','.join(failed) if failed else ''}; "
              "capability AVAILABLE for contract (profile policy design = T30)")


# ---------------------------------------------------------------- perf

def cmd_perf(ev: Evidence):
    """Baseline numbers only: rootfs prepare, create+exec, exec python, delete,
    idle payload RSS + supervision observation. No tuning."""
    base = STATE / "bundles" / "a"
    if not (base / "rootfs").is_dir():
        raise SystemExit("run unpack first")
    prepare_nobody_context()
    chown_tree(base, NOBODY_UID, NOBODY_GID)

    runs = []
    for i in range(3):
        dst = STATE / f"perf-copy{i}"
        shutil.rmtree(dst, ignore_errors=True)
        os.makedirs(dst, exist_ok=True)
        t0 = time.monotonic()
        run(["/bin/cp", "-a", str(base) + "/.", str(dst)])
        runs.append(int((time.monotonic() - t0) * 1000))
        shutil.rmtree(dst)
    du = run(["/usr/bin/du", "-sb", str(base / "rootfs")])["stdout"].split()[0]
    ev.step("rootfs prepare FULL_COPY", {"runs_ms": runs, "rootfs_bytes": int(du)})

    dst = STATE / "perf-del"
    shutil.rmtree(dst, ignore_errors=True)
    os.makedirs(dst, exist_ok=True)
    os.chown(dst, NOBODY_UID, NOBODY_GID)
    run(["/bin/cp", "-a", str(base) + "/.", str(dst)])
    t0 = time.monotonic()
    res = run_as(NOBODY_UID, NOBODY_GID, ["/bin/bash", "-c",
                 f"/bin/rm -rf --one-file-system {dst}/rootfs {dst}/config.json"])
    ev.step("sandbox delete (rootfs contents as uid 65534)",
            {"exit": res["exit"], "ms": int((time.monotonic() - t0) * 1000)})

    for label, args, n in (("create+exec /bin/true", ["/bin/true"], 5),
                           ("create+exec python3 -c pass", ["/usr/bin/python3", "-c", "pass"], 3)):
        cfg = minimal_config(args, host_uid=NOBODY_UID, host_gid=NOBODY_GID,
                             bind_proc_ro=True)
        (base / "config.json").write_text(json.dumps(cfg, indent=1))
        runs, exits = [], []
        for i in range(n):
            t0 = time.monotonic()
            res = run_as(NOBODY_UID, NOBODY_GID,
                         [RUNC, "run", "--bundle", str(base), f"perf-{i}"],
                         timeout=60)
            runs.append(int((time.monotonic() - t0) * 1000))
            exits.append(res["exit"])
        ev.step(label, {"runs_ms": runs, "exits": exits})

    pid, _ = start_detached(ev, base, "perf-idle", ["/bin/sleep", "600"])
    if pid:
        st = open(f"/proc/{pid}/status").read()
        keep = [l for l in st.splitlines() if l.startswith(("VmRSS", "VmSize", "VmData"))]
        ppid = open(f"/proc/{pid}/stat").read().split()[3]
        sup = run(["/bin/bash", "-c",
                   "for p in /proc/[0-9]*/comm; do c=$(cat $p 2>/dev/null); "
                   "[ \"$c\" = runc ] && echo x; done | wc -l"])
        ev.step("idle sandbox (sleep 600 payload)", {
            "payload_host_pid": pid, "payload_status": keep, "ppid": ppid,
            "resident_runc_supervisors": int(sup["stdout"].strip() or 0),
            "note": "detached runc exits after start; payload reparents (see ppid). A "
                    "resident per-sandbox lifecycle supervisor is an explicit architecture "
                    "block NOT provided by runc in this mode"})
    stop_detached("perf-idle")
    ev.finish("performance baseline recorded (no tuning applied)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("experiment",
                        choices=("fixtures", "unpack", "firstrun", "p1", "p2", "uids",
                                 "rootfs", "isolation", "p21", "seccomp", "perf"))
    args = parser.parse_args()
    STATE.mkdir(parents=True, exist_ok=True)
    ev = Evidence(args.experiment)
    {"fixtures": cmd_fixtures, "unpack": cmd_unpack, "firstrun": cmd_firstrun,
     "p1": cmd_p1, "p2": cmd_p2, "uids": cmd_uids,
     "rootfs": cmd_rootfs, "isolation": cmd_isolation, "p21": cmd_p21,
     "seccomp": cmd_seccomp,
     "perf": cmd_perf}[args.experiment](ev)


if __name__ == "__main__":
    main()
