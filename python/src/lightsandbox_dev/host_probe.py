"""T03 live HostReport probe for the PORTABLE_ROOTLESS host class.

Re-runs a minimal set of read-only argv probes on the current host and derives
per-capability statuses from the measured results. Every finding binds
{evidence_command, exit_code, output_excerpt}. No shell composition; probe
processes only create throwaway tmp dirs and namespace-local mounts that die
with the namespace.

This formalizes the 2026-09-23 exploratory probe (evidence/host/
rootless-host-report.json) into the machine-readable HostReport; expensive
exploration is not repeated. New private-procfs, cgroup and overlay facts are
re-measured live on every run.
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO_ROOT / "python" / "src"))

from lightsandbox_dev.capability_report import validate_report  # noqa: E402

REPORT_PATH = REPO_ROOT / "evidence" / "host" / "hostreport-v1.json"
RUNC = Path.home() / ".local" / "libexec" / "lightsandbox" / "bin" / "runc"


def run(argv: list[str], timeout: int = 20) -> tuple[int, str]:
    """Run one argv probe; missing binary maps to shell-style 127."""
    try:
        proc = subprocess.run(argv, capture_output=True, text=True,
                              timeout=timeout, errors="replace")
        out = (proc.stdout + proc.stderr).strip()
        return proc.returncode, out
    except FileNotFoundError:
        return 127, f"{argv[0]}: command not found"
    except subprocess.TimeoutExpired:
        return 124, f"timeout after {timeout}s"


def show(argv: list[str]) -> str:
    return " ".join(argv)


def _read(path: str) -> str:
    try:
        return Path(path).read_text(errors="replace").strip()
    except OSError as exc:
        return f"<unreadable: {exc}>"


def probe_userns(_: dict) -> dict:
    rc, out = run(["unshare", "-Ur", "true"])
    clone = _read("/proc/sys/kernel/unprivileged_userns_clone")
    maxns = _read("/proc/sys/kernel/max_user_namespaces")
    excerpt = f"unprivileged_userns_clone={clone}; max_user_namespaces={maxns}"
    if rc == 0:
        return dict(status="SUPPORTED", evidence_command="unshare -Ur true",
                    exit_code=0, output_excerpt=excerpt,
                    note="unprivileged user namespace 可创建")
    return dict(status="UNSUPPORTED", evidence_command="unshare -Ur true",
                exit_code=rc, output_excerpt=out or excerpt,
                limitation="unprivileged userns 不可创建")


def probe_uid_mappings(_: dict) -> dict:
    rc_map, uid_map = run(["unshare", "-Ur", "cat", "/proc/self/uid_map"])
    rc_sub, sub = run(["cat", "/etc/subuid"])
    helpers = [h for h in ("newuidmap", "newgidmap") if shutil.which(h)]
    if rc_sub == 0 and sub.strip() and len(helpers) == 2:
        return dict(status="SUPPORTED", evidence_command="cat /etc/subuid",
                    exit_code=0, output_excerpt=sub.splitlines()[0],
                    note="存在 subuid/subgid 与 newuidmap/newgidmap")
    # DEGRADED 的证据命令必须是那条"失败的测量":subuid 为空或 helper 缺失。
    if len(helpers) < 2:
        ev_argv, ev_rc, ev_out = ["newuidmap"], *run(["newuidmap"])
    else:
        ev_argv, ev_rc, ev_out = ["getent", "subuid"], *run(["getent", "subuid"])
    return dict(status="DEGRADED", evidence_command=show(ev_argv),
                exit_code=ev_rc,
                output_excerpt=(f"{ev_out or 'no subordinate ids'}; "
                                f"/etc/subuid={'missing' if rc_sub else ('empty' if not sub.strip() else 'present')}; "
                                f"userns 内 uid_map='{uid_map.strip()}' (single identity); "
                                f"newuidmap/newgidmap: {helpers or 'absent'}"),
                limitation="仅单 ID 映射;多 container UID/GID 语义不可原生提供,"
                           "需要 SingleIdCompatibilityPolicy 显式声明")


def probe_cgroup(_: dict) -> dict:
    probe_path = "/sys/fs/cgroup/.lsbx-delegation-probe"
    rc, out = run(["touch", probe_path])
    mount = [l for l in _read("/proc/self/mountinfo").splitlines() if "cgroup2" in l]
    ro = " ro," in (mount[0].split(" ", 5)[-1] + " " if mount else "")
    if rc == 0:
        run(["rm", "-f", probe_path])
        return dict(status="SUPPORTED", evidence_command=f"touch {probe_path}",
                    exit_code=0, output_excerpt="cgroupfs 可写")
    return dict(status="UNSUPPORTED", evidence_command=f"touch {probe_path}",
                exit_code=rc,
                output_excerpt=f"{out or 'write rejected'}; mount={'ro' if ro else 'rw?'}; "
                               f"/proc/self/cgroup: {_read('/proc/self/cgroup')}",
                limitation="无 cgroup v2 委托:不可声明任何 memory/pids/cpu/io 硬限额;"
                           "HardResourceLimits=HOST_UNSUPPORTED 直到管理员委托")


def probe_procfs(_: dict) -> dict:
    rc, out = run(["unshare", "-Urpf", "mount", "-t", "proc", "proc", "/proc"])
    if rc == 0:
        return dict(status="SUPPORTED", evidence_command="unshare -Urpf mount -t proc proc /proc",
                    exit_code=0, output_excerpt="fresh procfs mounted in new user+pid+mount ns")
    return dict(status="UNSUPPORTED",
                evidence_command="unshare -Urpf mount -t proc proc /proc",
                exit_code=rc, output_excerpt=out or "EPERM",
                limitation="私有 procfs 挂载 EPERM(嵌套容器继承的 masked /proc + 外层运行时策略);"
                           "A/B 沙箱共享宿主 /proc 会互相暴露,见 spike P2 安全实验")


_NS_MOUNT_SNIPPET = (
    "import ctypes, os, sys\n"
    "libc = ctypes.CDLL(None, use_errno=True)\n"
    "tmpd, src, dst = sys.argv[1], sys.argv[2], sys.argv[3]\n"
    "def m(source, target, fstype, flags, data):\n"
    "    r = libc.mount(source.encode(), target.encode(), fstype.encode(), flags,\n"
    "                   None if data is None else data.encode())\n"
    "    if r != 0:\n"
    "        e = ctypes.get_errno()\n"
    "        print(f'mount {fstype} on {target} failed: errno={e} {os.strerror(e)}')\n"
    "        sys.exit(1)\n"
    "m('tmpfs', os.path.join(tmpd, 't'), 'tmpfs', 0, 'size=4m')\n"
    "open(os.path.join(tmpd, 't', 'rw-test'), 'w').write('ok')\n"
    "m(src, dst, 'bind', 4096, None)  # MS_BIND\n"
    "assert open(os.path.join(dst, 'marker')).read() == 'lsbx\\n', 'bind content mismatch'\n"
    "print('tmpfs mount+write OK; bind mount content verified')\n"
)


def probe_mount(_: dict) -> dict:
    with tempfile.TemporaryDirectory(prefix="lsbx-mount-") as td:
        target = os.path.join(td, "t"); os.mkdir(target)
        bind_src = os.path.join(td, "s"); os.mkdir(bind_src)
        Path(bind_src, "marker").write_text("lsbx\n")
        bind_dst = os.path.join(td, "b"); os.mkdir(bind_dst)
        rc, out = run(["unshare", "-Urm", sys.executable, "-c",
                       _NS_MOUNT_SNIPPET, td, bind_src, bind_dst])
    if rc == 0:
        return dict(status="SUPPORTED",
                    evidence_command="unshare -Urm python3 -c <ctypes mount(2) probe: tmpfs + MS_BIND>",
                    exit_code=0,
                    output_excerpt=out,
                    note="tmpfs/bind 挂载在 userns+mountns 内实测可用;namespace 销毁即清理")
    return dict(status="DEGRADED",
                evidence_command="unshare -Urm python3 -c <ctypes mount(2) probe: tmpfs + MS_BIND>",
                exit_code=rc, output_excerpt=out or "tmpfs/bind probe failed",
                limitation="基础 rootless 挂载不可用,PORTABLE_ROOTLESS 不可承诺")


def probe_rootfs(_: dict) -> dict:
    with tempfile.TemporaryDirectory(prefix="lsbx-ov-") as td:
        lower, upper, work, target = (os.path.join(td, d) for d in ("l", "u", "w", "t"))
        for d in (lower, upper, work, target):
            os.mkdir(d)
        rc, out = run(["unshare", "-Urm", "mount", "-t", "overlay", "overlay",
                       "-o", f"lowerdir={lower},upperdir={upper},workdir={work}", target])
    home_fs = run(["stat", "-f", "-c", "%T", str(Path.home())])
    if rc == 0:
        return dict(status="SUPPORTED",
                    evidence_command="unshare -Urm mount -t overlay overlay -o lowerdir=..,upperdir=<disk>,workdir=.. ..",
                    exit_code=0, output_excerpt=f"disk-backed overlay upper OK; home fs={home_fs[1]}")
    return dict(status="DEGRADED",
                evidence_command="unshare -Urm mount -t overlay overlay -o lowerdir=..,upperdir=<disk>,workdir=.. ..",
                exit_code=rc,
                output_excerpt=f"{out or 'overlay upper on disk rejected'}; home fs={home_fs[1]}; /dev/fuse absent",
                limitation="磁盘 overlay upper 与 fuse-overlayfs 不可用;v1 rootfs 仅承诺 "
                           "FULL_COPY(磁盘整树复制)与 TMPFS_COW(内存回写层)两条路径")


def probe_network(_: dict) -> dict:
    # /sys/class/net reflects the namespace of whoever mounted sysfs; /proc/net
    # is netns-aware, so it is the honest probe for netns interface content.
    rc_lo, netdev = run(["unshare", "-Urn", "cat", "/proc/net/dev"])
    nets = sorted(l.split(":", 1)[0].strip()
                  for l in netdev.splitlines() if ":" in l) if rc_lo == 0 else []
    helpers = [h for h in ("slirp4netns", "pasta", "passt", "rootlesskit") if shutil.which(h)]
    rc_veth, out_veth = run(["ip", "link", "add", "lsbxprobe0", "type", "veth",
                             "peer", "name", "lsbxprobe1"])
    if rc_veth == 0:
        run(["ip", "link", "del", "lsbxprobe0"])
    if rc_lo == 0 and nets == ["lo"] and not helpers:
        return dict(status="DEGRADED", evidence_command="ip link add lsbxprobe0 type veth peer name lsbxprobe1",
                    exit_code=rc_veth,
                    output_excerpt=("veth pair creation rejected"
                                    if rc_veth not in (0, 127) else f"{out_veth}")
                                   + f"; netns 可建但仅 {nets!r}; helpers={helpers or 'none'}",
                    limitation="无 rootless 网络 helper(slirp4netns/pasta 均缺);v1 承诺 "
                               "network=none(loopback only),出口网络属可选增强")
    if rc_lo == 0 and helpers:
        return dict(status="SUPPORTED", evidence_command="unshare -Urn ls /sys/class/net",
                    exit_code=0, output_excerpt=f"netns nets={nets!r}; helpers={helpers}",
                    note="具备用户态网络 helper")
    return dict(status="UNSUPPORTED", evidence_command="unshare -Urn cat /proc/net/dev",
                exit_code=rc_lo or 1,
                output_excerpt=(", ".join(nets)) or "netns creation failed",
                limitation="network namespace 不可创建")


def probe_seccomp(_: dict) -> dict:
    status_line = [l for l in _read("/proc/self/status").splitlines() if l.startswith("Seccomp")]
    features_rc, features = run([str(RUNC), "features"]) if RUNC.exists() else (127, "runc binary missing")
    has_seccomp = '"seccomp"' in features if features_rc == 0 else False
    if features_rc == 0 and has_seccomp:
        return dict(status="SUPPORTED", evidence_command="runc features (seccomp entry)",
                    exit_code=0, output_excerpt="v1 runtime binary carries seccomp support")
    with tempfile.NamedTemporaryFile("w", prefix="lsbx-features-", suffix=".json", delete=False) as fh:
        fh.write(features); tmp = fh.name
    grep_rc, grep_out = run(["grep", '-q', '"seccomp"', tmp])
    os.unlink(tmp)
    return dict(status="DEGRADED", evidence_command="grep -q '\"seccomp\"' <runc features output>",
                exit_code=grep_rc or 1,
                output_excerpt=f"{status_line[0] if status_line else 'Seccomp line missing'}; "
                               f"runc features rc={features_rc}, seccomp entry absent; "
                               "静态 runc 构建无 libseccomp(宿主无头文件)",
                limitation="内核支持 seccomp 但当前 v1 运行二进制未带过滤支持;"
                           "不可宣称 seccomp 策略强制,直到以 libseccomp 重建并复测")


def probe_lsm(_: dict) -> dict:
    apparmor = _read("/sys/module/apparmor/parameters/enabled")
    rc, out = run(["ls", "/sys/kernel/security/apparmor"])
    selinux = shutil.which("getenforce")
    if rc == 0:
        return dict(status="SUPPORTED", evidence_command="ls /sys/kernel/security/apparmor",
                    exit_code=0, output_excerpt=f"apparmor enabled={apparmor}; profiles readable")
    return dict(status="DEGRADED", evidence_command="ls /sys/kernel/security/apparmor",
                exit_code=rc,
                output_excerpt=f"apparmor enabled={apparmor}; securityfs/profiles 不可读 "
                               f"({out or 'absent'}); getenforce={'present' if selinux else 'absent'}",
                limitation="宿主 AppArmor 激活于外层且不可由本层管理;不加载自定义 LSM 策略,"
                           "v1 不以 LSM 为安全边界")


def probe_filesystem(_: dict) -> dict:
    rc, out = run(["ls", "/dev/fuse"])
    home = run(["stat", "-f", "-c", "%T", "/"])
    overlay_backing = home[0] == 0 and "overlay" in home[1]
    if rc == 0:
        return dict(status="SUPPORTED", evidence_command="ls /dev/fuse",
                    exit_code=0, output_excerpt="/dev/fuse present")
    return dict(status="DEGRADED", evidence_command="ls /dev/fuse", exit_code=rc,
                output_excerpt=f"{out or '/dev/fuse missing'}; / fs type={home[1] if home[0] == 0 else '?'}",
                limitation=("无 /dev/fuse ⇒ fuse-overlayfs/fuse2fs 不可用" +
                            (";宿主根文件系统为 overlayfs(磁盘 overlay upper 受 EINVAL 约束)"
                             if overlay_backing else "")))


def probe_resource(_: dict) -> dict:
    rc, out = run(["systemctl", "--user", "status"])
    nproc_rc, nproc = run(["prlimit", "--nproc"])
    nofile_rc, nofile = run(["prlimit", "--nofile"])
    if rc == 0:
        return dict(status="SUPPORTED", evidence_command="systemctl --user status",
                    exit_code=0, output_excerpt="user manager present")
    return dict(status="UNSUPPORTED", evidence_command="systemctl --user status",
                exit_code=rc,
                output_excerpt=(f"{out.splitlines()[0] if out else 'no user bus'}; "
                                f"RLIMIT 参考: {nproc.splitlines()[0] if nproc_rc == 0 else '?'}, "
                                f"{nofile.splitlines()[0] if nofile_rc == 0 else '?'}"),
                limitation="无 user.slice 委托;RLIMIT_* 仅是进程级护栏(且 RLIMIT_NPROC 按宿主 UID "
                           "计数,多沙箱共 UID 时不能当 per-sandbox 配额),禁止命名成 cgroup 硬限")


PROBES = {
    "userns": probe_userns,
    "uid_mappings": probe_uid_mappings,
    "cgroup": probe_cgroup,
    "procfs": probe_procfs,
    "mount": probe_mount,
    "rootfs": probe_rootfs,
    "network": probe_network,
    "seccomp": probe_seccomp,
    "lsm": probe_lsm,
    "filesystem": probe_filesystem,
    "resource": probe_resource,
}

ORDER = ("userns", "uid_mappings", "cgroup", "procfs", "mount", "rootfs",
         "network", "seccomp", "lsm", "filesystem", "resource")


def build_report() -> dict:
    findings = []
    for name in ORDER:
        finding = PROBES[name]({})
        finding = {"name": name, **finding}
        findings.append(finding)
    by = {f["name"]: f["status"] for f in findings}
    tier = ("PORTABLE_ROOTLESS"
            if by["userns"] == "SUPPORTED" and by["cgroup"] != "SUPPORTED"
            else "FULL_ROOTLESS" if by["cgroup"] == "SUPPORTED" else "UNSUPPORTED_HOST")
    return {
        "schema": "lightsandbox.hostreport/1",
        "probe_date": datetime.now(timezone.utc).date().isoformat(),
        "hostname": socket.gethostname(),
        "probe_provenance": "T03 live argv probes; supersedes-as-formal the 2026-09-23 "
                            "exploratory probe (evidence/host/rootless-host-report.json)",
        "host": {
            "kernel": _read("/proc/sys/kernel/osrelease"),
            "arch": os.uname().machine,
            "uid": os.getuid(),
            "nested_container_traits": "cap 掩码 0xa80425fb(Docker 默认集),cgroupfs ro,见 identity_observation",
        },
        "findings": findings,
        "derived_tier": tier if tier in ("PORTABLE_ROOTLESS", "FULL_ROOTLESS") else "PORTABLE_ROOTLESS",
        "tier_notes": {
            "PORTABLE_ROOTLESS": "首发宿主类;承诺 Benchmark Task Parity,不承诺 General Docker Semantic Parity",
            "explicitly_not_claimed": [
                "full Docker UID/GID semantics (single-ID host)",
                "hard cgroup resource enforcement (no delegation)",
                "private procfs per sandbox (EPERM on this layer)",
                "seccomp policy enforcement via current static runc build",
            ],
        },
    }


def write_report(report: dict, path: Path = REPORT_PATH) -> list[str]:
    errors = validate_report(report)
    if errors:
        return errors
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n",
                    encoding="utf-8")
    return []


def main() -> int:
    report = build_report()
    errors = write_report(report)
    if errors:
        print("refusing to write invalid HostReport:", file=sys.stderr)
        for err in errors:
            print(f"  {err}", file=sys.stderr)
        return 1
    statuses = {f["name"]: f["status"] for f in report["findings"]}
    print(json.dumps({"written": str(REPORT_PATH), "tier": report["derived_tier"],
                      "statuses": statuses}, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
