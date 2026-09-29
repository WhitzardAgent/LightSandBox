#!/usr/bin/env python3
"""Worker-side candidate-only benchmark case runner.

This runner performs real asset checks and isolated OCI image preparation. It
never substitutes a verifier, and it records a blocked stage when the native
case driver or an upstream asset is unavailable. A run is not parity evidence.
"""
from __future__ import annotations

import argparse
import base64
import fcntl
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import time
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CASES = {
    "swebench:full:astropy__astropy-11693": ("swebench", 3094),
    "terminalbench:tasks:atrx-vep-crispr": ("terminalbench", 66),
    "cybergym:main:arvo:47101": ("cybergym", 1507),
}
MAX_WORKER_BYTES = 100 * 1024**3


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def source_state() -> dict:
    def git(*args: str) -> str:
        p = subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True)
        if p.returncode:
            raise RuntimeError(f"git {' '.join(args)} exited {p.returncode}")
        return p.stdout.decode("utf-8", "replace").strip()

    diff = subprocess.run(["git", "-C", str(ROOT), "diff", "--binary"], capture_output=True)
    if diff.returncode:
        raise RuntimeError("git diff failed")
    return {"commit": git("rev-parse", "HEAD"),
            "status": git("status", "--porcelain"),
            "tracked_diff_sha256": hashlib.sha256(diff.stdout).hexdigest(),
            "runner_sha256": sha256(Path(__file__))}


def worker_size(root: Path) -> int:
    # `du` can race a concurrently removed build temp file. In that case its
    # reported total is still useful, but retry before accepting it. Keep the
    # largest observed total so a disappearing file cannot lower the budget
    # check during this sample.
    observed = []
    for attempt in range(3):
        p = subprocess.run(["du", "-sk", str(root)], capture_output=True, text=True)
        if p.stdout.split():
            observed.append(int(p.stdout.split()[0]) * 1024)
        if p.returncode == 0:
            return max(observed)
        if "No such file or directory" not in p.stderr or not observed:
            raise RuntimeError(f"du failed: {p.stderr.strip()}")
        if attempt < 2:
            time.sleep(0.1)
    return max(observed)


def run_command(argv: list[str], cwd: Path, log_dir: Path, name: str,
                timeout: int = 1800, env: dict | None = None,
                budget_root: Path | None = None) -> dict:
    stdout = log_dir / f"{name}.stdout"
    stderr = log_dir / f"{name}.stderr"
    started = datetime.now(timezone.utc).isoformat()
    start = time.monotonic()
    termination = "EXITED"
    with stdout.open("wb") as out, stderr.open("wb") as err:
        try:
            p = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                 stdout=out, stderr=err, start_new_session=True)
            while p.poll() is None:
                if time.monotonic() - start > timeout:
                    termination = "TIMEOUT"
                    os.killpg(p.pid, signal.SIGKILL)
                    break
                if budget_root is not None and worker_size(budget_root) > MAX_WORKER_BYTES:
                    termination = "BUDGET_EXCEEDED"
                    os.killpg(p.pid, signal.SIGKILL)
                    break
                time.sleep(2)
            code = p.wait()
        except OSError as exc:
            err.write(f"spawn failed: {exc}\n".encode())
            code = None
            termination = "SPAWN_FAILED"
    return {"argv": argv, "cwd": str(cwd), "started_at": started,
            "duration_ms": int((time.monotonic() - start) * 1000),
            "exit_code": code, "termination": termination,
            "stdout": str(stdout), "stderr": str(stderr),
            "stdout_sha256": sha256(stdout), "stderr_sha256": sha256(stderr)}


def inspect_assets(uid: str, assets: Path) -> tuple[dict, list[Path], str | None]:
    if uid.startswith("swebench:"):
        row_path = assets / "swebench/full/astropy__astropy-11693.json"
        if not row_path.is_file():
            return {}, [], f"official dataset row missing: {row_path}"
        row = json.loads(row_path.read_text())
        if row.get("instance_id") != "astropy__astropy-11693" or not row.get("eval_script") or not row.get("patch"):
            return {}, [row_path], "SWE-bench row lacks original patch/eval script"
        return {"source_row_sha256": sha256(row_path), "source_revision":
                "c6fe717fd7a4c3ac1daa4055a4fd082c6a1d28a2",
                "image_reference": row["image"], "base_commit": row["base_commit"],
                "gold_patch_sha256": hashlib.sha256(row["patch"].encode()).hexdigest(),
                "eval_script_sha256": hashlib.sha256(row["eval_script"].encode()).hexdigest()}, [row_path], None
    if uid.startswith("terminalbench:"):
        task = assets / "terminalbench/tasks/atrx-vep-crispr"
        required = [task / p for p in ("environment/Dockerfile", "tests/Dockerfile",
                                         "tests/test.sh", "tests/test_outputs.py",
                                         "solution/solve.sh", "solution/solve.py", "task.toml")]
        missing = [str(p) for p in required if not p.is_file()]
        if missing:
            return {}, required, f"original Terminal-Bench files missing: {missing}"
        files = sorted(p for p in task.rglob("*") if p.is_file())
        return {"source_revision": "4def1f367467b34b18e0dbdc086400ba71c3e037",
                "files": {str(p.relative_to(task)): sha256(p) for p in files},
                "declared_artifact": "/app/output/mutation.report.json"}, files, None
    row_path = assets / "cybergym/arvo-47101.json"
    if not row_path.is_file():
        return {}, [], f"official CyberGym task row missing: {row_path}"
    row = json.loads(row_path.read_text())
    if row.get("task_id") != "arvo:47101":
        return {}, [row_path], "CyberGym task row identity mismatch"
    task_dir = assets / "cybergym/data/arvo/47101"
    required = [task_dir / p for p in ("repo-vul.tar.gz", "repo-fix.tar.gz",
                                         "description.txt", "error.txt", "patch.diff")]
    missing = [str(p) for p in required if not p.is_file()]
    if missing:
        return {"source_row_sha256": sha256(row_path), "source_revision":
                "bde190ded494e52bc684b66073b436c9d992c7c6",
                "missing_assets": missing}, [row_path], f"official CyberGym assets missing: {missing}"
    receipt_path = ROOT / "evidence/runtime/ws03/cybergym-47101-assets.json"
    if not receipt_path.is_file():
        return {}, [row_path, *required], "CyberGym LFS source checksum receipt missing"
    receipt = json.loads(receipt_path.read_text())
    if receipt.get("revision") != "bde190ded494e52bc684b66073b436c9d992c7c6":
        return {}, [row_path, *required], "CyberGym LFS source revision mismatch"
    expected = {Path(f["path"]).name: f for f in receipt["files"]}
    for path in required:
        bound = expected.get(path.name)
        if not bound or path.stat().st_size != bound["size"] or sha256(path) != bound["sha256"]:
            return {}, [row_path, *required], f"CyberGym LFS asset mismatch: {path.name}"
    return {"source_row_sha256": sha256(row_path), "source_revision":
            "bde190ded494e52bc684b66073b436c9d992c7c6",
            "lfs_receipt_sha256": sha256(receipt_path),
            "files": {p.name: sha256(p) for p in required},
            "vulnerable_image": "n132/arvo:47101-vul",
            "fixed_image": "n132/arvo:47101-fix"}, [row_path, *required], None


def build_specs(uid: str, assets: Path, case_dir: Path, metadata: dict) -> list[tuple[str, Path, Path]]:
    if uid.startswith("terminalbench:"):
        task = assets / "terminalbench/tasks/atrx-vep-crispr"
        return [("agent", task / "environment", task / "environment"),
                ("verifier", task / "tests", task / "tests")]
    dockerfile_dir = case_dir / "source-image"
    dockerfile_dir.mkdir()
    refs = [metadata["image_reference"]] if uid.startswith("swebench:") else [
        metadata["vulnerable_image"], metadata["fixed_image"]]
    result = []
    for i, ref in enumerate(refs):
        if not re.fullmatch(r"[A-Za-z0-9_./:-]+", ref):
            raise RuntimeError("unsafe image reference in locked row")
        target = dockerfile_dir / str(i)
        target.mkdir()
        (target / "Dockerfile").write_text(f"FROM {ref}\n")
        result.append((f"source-{i}", target, target))
    return result


def reuse_prepared_images(uid: str, case_dir: Path, worker_root: Path,
                          metadata: dict, commands: list[dict]) -> bool:
    """Reuse verified OCI exports for the exact same locked task assets.

    Upstream tags are mutable, so the first successful export's bytes and
    manifest remain fixed for later candidate attempts on this worker.
    """
    names = ("agent", "verifier") if uid.startswith("terminalbench:") else (
        ("source-0",) if uid.startswith("swebench:") else ("source-0", "source-1"))
    previous_root = worker_root / "evidence/cases" / uid.replace(":", "_")
    if not previous_root.is_dir():
        return False
    source_keys = ("source_row_sha256", "source_revision", "files", "image_reference",
                   "vulnerable_image", "fixed_image", "lfs_receipt_sha256")
    for previous_dir in sorted(previous_root.iterdir()):
        if previous_dir == case_dir or not previous_dir.is_dir():
            continue
        receipt_path = previous_dir / "receipt.json"
        if not receipt_path.is_file():
            continue
        try:
            previous = json.loads(receipt_path.read_text())
            prior_assets = previous["assets"]
            if any(prior_assets.get(key) != metadata.get(key) for key in source_keys):
                continue
            files = [previous_dir / f"{name}.oci.tar" for name in names]
            expected = [prior_assets[f"{name}_oci_tar_sha256"] for name in names]
            if any(not path.is_file() or path.stat().st_size == 0 or sha256(path) != digest
                   for path, digest in zip(files, expected)):
                continue
        except (OSError, ValueError, KeyError, TypeError):
            continue
        for name, source, digest in zip(names, files, expected):
            target = case_dir / f"{name}.oci.tar"
            os.link(source, target)
            metadata[f"{name}_oci_tar_sha256"] = digest
            commands.append({"argv": ["cache-reuse", str(source), str(target)],
                             "exit_code": 0, "source_sha256": digest,
                             "reason": "same locked task assets and verified OCI bytes"})
        return True
    return False


def build_images(uid: str, assets: Path, case_dir: Path, worker_root: Path,
                 metadata: dict, commands: list[dict], proxy_url: str | None) -> str | None:
    buildkitd = worker_root / "bin/buildkitd"
    buildctl = worker_root / "bin/buildctl"
    if not buildkitd.is_file() or not buildctl.is_file():
        return "pinned isolated BuildKit binaries unavailable"
    if reuse_prepared_images(uid, case_dir, worker_root, metadata, commands):
        return None
    # AF_UNIX sun_path is 108 bytes on Linux. Keep the socket beside the
    # workspace root while retaining all durable build data in case_dir.
    socket_path = worker_root / f"bk-{os.getpid()}.sock"
    # Keep the content store across attempts. Large upstream images can take
    # longer than one SSH session to fetch; discarding verified blobs on a
    # failed attempt made the next real case start from zero again.
    cache_dir = worker_root / "buildkit-cache" / uid.replace(":", "_")
    cache_dir.mkdir(parents=True, exist_ok=True)
    build_root = cache_dir / "root"
    daemon_log = case_dir / "buildkitd.log"
    argv = ["unshare", "--user", "--map-root-user", "--mount", "--pid", "--fork",
            "--mount-proc", str(buildkitd), "--addr", f"unix://{socket_path}",
            "--root", str(build_root), "--oci-worker=true",
            "--containerd-worker=false", "--oci-worker-snapshotter=native"]
    env = os.environ.copy()
    env["HOME"] = str(case_dir)
    env["XDG_RUNTIME_DIR"] = str(worker_root)
    if proxy_url:
        env["HTTPS_PROXY"] = proxy_url
        env["https_proxy"] = proxy_url
    with (cache_dir / "lock").open("wb") as lock, daemon_log.open("wb") as log:
        fcntl.flock(lock, fcntl.LOCK_EX)
        started = datetime.now(timezone.utc).isoformat()
        proc = subprocess.Popen(argv, cwd=case_dir, env=env, stdin=subprocess.DEVNULL,
                                stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            for _ in range(100):
                if socket_path.exists() or proc.poll() is not None:
                    break
                time.sleep(0.1)
            commands.append({"argv": argv, "cwd": str(case_dir), "started_at": started,
                             "exit_code": proc.poll(), "log": str(daemon_log),
                             "log_sha256": sha256(daemon_log), "socket_ready": socket_path.exists()})
            if socket_path.exists():
                time.sleep(0.2)
            if not socket_path.exists() or proc.poll() is not None:
                return "isolated BuildKit daemon failed before socket readiness"
            for name, context, dockerfile in build_specs(uid, assets, case_dir, metadata):
                out = case_dir / f"{name}.oci.tar"
                cmd = [str(buildctl), "--addr", f"unix://{socket_path}", "build",
                       "--frontend", "dockerfile.v0", "--local", f"context={context}",
                       "--local", f"dockerfile={dockerfile}", "--output", f"type=oci,dest={out}"]
                rec = run_command(cmd, case_dir, case_dir, f"build-{name}",
                                  timeout=7200, env=env, budget_root=worker_root)
                commands.append(rec)
                if rec["exit_code"] != 0 or not out.is_file():
                    build_error = Path(rec["stderr"]).read_text(errors="replace")[-12000:]
                    if "failed to Lchown" in build_error and "subordinate IDs" in build_error:
                        metadata["capability_gap"] = "MULTI_UID_IMAGE_BUILD"
                        return ("original image requires multiple UID/GID ownership during build; "
                                "single-ID rootless BuildKit rejected it before sandbox creation; "
                                "UML image builder is not connected")
                    return f"original image preparation failed: {name}"
                metadata[f"{name}_oci_tar_sha256"] = sha256(out)
            return None
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait(timeout=5)
            socket_path.unlink(missing_ok=True)


def extract_oci_layout(archive: Path, dest: Path) -> str:
    """Unpack only OCI transport members, then bind its sole descriptor to runtime."""
    dest.mkdir()
    with tarfile.open(archive, "r:") as tf:
        for member in tf:
            parts = Path(member.name).parts
            if (not parts or Path(member.name).is_absolute() or ".." in parts
                    or not (member.isfile() or member.isdir())):
                raise RuntimeError(f"unsafe OCI archive member: {member.name}")
            if member.isdir():
                (dest / member.name).mkdir(parents=True, exist_ok=True)
                continue
            target = dest / member.name
            target.parent.mkdir(parents=True, exist_ok=True)
            with tf.extractfile(member) as src, target.open("xb") as out:
                shutil.copyfileobj(src, out)
    index_path = dest / "index.json"
    index = json.loads(index_path.read_text())
    manifests = index.get("manifests", [])
    if len(manifests) != 1:
        raise RuntimeError(f"OCI export has {len(manifests)} descriptors; expected one")
    descriptor = manifests[0]
    digest = descriptor["digest"]
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise RuntimeError("OCI manifest digest is malformed")
    manifest_blob = dest / "blobs/sha256" / digest.split(":", 1)[1]
    if sha256(manifest_blob) != digest.split(":", 1)[1]:
        raise RuntimeError("OCI manifest blob digest mismatch")
    descriptor.setdefault("annotations", {})["org.opencontainers.image.ref.name"] = "runtime"
    index_path.write_text(json.dumps(index, sort_keys=True, separators=(",", ":")))
    return digest


def guest_upload_step(name: str, data: bytes, target: str) -> dict:
    encoded = base64.b64encode(data).decode("ascii")
    if len(encoded) > 100_000:
        raise RuntimeError(f"{name} exceeds argv upload limit; native files transport required")
    # Data is base64 alphabet only; the shell command is fixed by the adapter.
    command = f"printf %s '{encoded}' | base64 -d > '{target}'"
    return {"name": name, "argv": ["/bin/bash", "-lc", command],
            "timeout_ms": 20_000, "require_exit_zero": True}


def swe_harness_eval_script(raw: str) -> str:
    """Apply pinned SWE-bench make_test_spec's script shaping, unchanged.

    Source: 02e7a74ffd0b707aab73d203fe87bdc7c76afc8e,
    swebench/harness/utils.py (sha256 c22f38fdd4ffd34301da7f837b82926cf261d81ca0387d92dd2990b393517c79).
    """
    lines = [line for line in raw.strip().split("\n")
             if line not in ("#!/bin/bash", "set -uxo pipefail")]
    for i, line in enumerate(lines):
        if ">>>>> End Test Output" in line:
            lines = [*lines[:i], "SWEBENCH_TEST_EXIT_CODE=$?", line,
                     'echo ">>>>> Test Exit Code: $SWEBENCH_TEST_EXIT_CODE"',
                     *lines[i + 1:]]
            break
    return "\n".join(["#!/bin/bash", "set -uxo pipefail", *lines]) + "\n"


def native_swe_case(row: dict, layout: Path, case_dir: Path, worker_root: Path,
                    patch: str, label: str, commands: list[dict]) -> dict:
    driver = worker_root / "target/debug/ls-case-driver"
    if not driver.is_file():
        raise RuntimeError("native case driver binary missing")
    request_path = case_dir / f"{label}.request.json"
    receipt_path = case_dir / f"{label}.native-receipt.json"
    patch_bytes = patch.encode()
    eval_script = swe_harness_eval_script(row["eval_script"])
    # Match the upstream harness patch-application fallbacks. This adapter
    # changes the transport and records each shell command's raw log; it does
    # not alter the model patch or test script.
    apply_patch = """set -x
attempt=0
for command in 'git apply --verbose' 'git apply --verbose --3way' 'git apply --verbose --reject' 'patch --batch --forward --fuzz=5 -p1 -i'; do
  if [ "$attempt" -ne 0 ]; then
    git checkout -- . ; git clean -fd
  fi
  if $command /tmp/patch.diff; then
    exit 0
  fi
  attempt=$((attempt + 1))
done
git apply --check --reverse /tmp/patch.diff
"""
    steps = [guest_upload_step("upload-patch", patch_bytes, "/tmp/patch.diff"),
             {"name": "patch-digest", "argv": ["sha256sum", "/tmp/patch.diff"],
              "timeout_ms": 20_000, "require_exit_zero": True},
             {"name": "apply-patch", "argv": ["/bin/bash", "-lc",
              apply_patch], "cwd": "/testbed",
              "timeout_ms": 120_000, "require_exit_zero": True}]
    if label == "gold":
        steps += [guest_upload_step("upload-eval-script", eval_script.encode(), "/eval.sh"),
                  {"name": "eval-script-digest", "argv": ["sha256sum", "/eval.sh"],
                   "timeout_ms": 20_000, "require_exit_zero": True},
                  {"name": "original-eval", "argv": ["/bin/bash", "-lc", "/bin/bash /eval.sh 2>&1"],
                   "timeout_ms": 1_800_000, "require_exit_zero": False}]
    request = {"layout": str(layout), "workspace": str(case_dir / f"native-{label}"),
               "runc": str(worker_root / "bin/runc"),
               "umoci": str(worker_root / "bin/umoci"),
               "guest_init": str(worker_root / "target/x86_64-unknown-linux-musl/debug/lsguest-init"),
               "command_override": ["tail", "-f", "/dev/null"],
               "steps": steps}
    request_path.write_text(json.dumps(request, indent=2))
    record = run_command([str(driver), str(request_path), str(receipt_path)],
                         case_dir, case_dir, f"native-{label}",
                         timeout=2_100, budget_root=worker_root)
    commands.append(record)
    if not receipt_path.is_file():
        raise RuntimeError(f"{label} native driver lost its receipt")
    native = json.loads(receipt_path.read_text())
    native["receipt_path"] = str(receipt_path)
    native["receipt_sha256"] = sha256(receipt_path)
    if native.get("steps"):
        try:
            verify_guest_digest(native, "patch-digest", hashlib.sha256(patch_bytes).hexdigest())
            if label == "gold" and any(s.get("name") == "eval-script-digest" for s in native["steps"]):
                verify_guest_digest(native, "eval-script-digest",
                                    hashlib.sha256(eval_script.encode()).hexdigest())
        except RuntimeError as exc:
            native["complete"] = False
            native["transfer_error"] = str(exc)
    return native


def run_swe(uid: str, assets: Path, case_dir: Path, worker_root: Path,
            metadata: dict, commands: list[dict]) -> dict:
    row_path = assets / "swebench/full/astropy__astropy-11693.json"
    row = json.loads(row_path.read_text())
    layout = case_dir / "oci-layout"
    metadata["candidate_oci_manifest_digest"] = extract_oci_layout(
        case_dir / "source-0.oci.tar", layout)
    gold = native_swe_case(row, layout, case_dir, worker_root, row["patch"], "gold", commands)
    # The deliberately malformed patch is an explicit negative control. It
    # must not reach the original verifier and must leave no live sandbox.
    bad_patch = "diff --git a/not-a-real-file b/not-a-real-file\n@@ malformed @@\n"
    wrong = native_swe_case(row, layout, case_dir, worker_root, bad_patch, "wrong", commands)
    outcome = {"gold": gold, "wrong_patch": wrong,
               "wrong_patch_rejected": (
                   not wrong.get("complete") and
                   "apply-patch" in (wrong.get("step_error") or "") and
                   wrong.get("cleanup", {}).get("all_ok") is True and
                   not any(s.get("name") == "original-eval" for s in wrong.get("steps", [])))}
    eval_steps = [s for s in gold.get("steps", []) if s.get("name") == "original-eval"]
    if gold.get("complete") and len(eval_steps) == 1 and not eval_steps[0].get("truncated"):
        grade_path = case_dir / "swebench-grade.json"
        cmd = [str(worker_root / "venv/swebench/bin/python"),
               str(ROOT / "dev/swebench_grade.py"),
               "--upstream", str(worker_root / "src/swebench"),
               "--row", str(row_path), "--log", eval_steps[0]["stdout"],
               "--output", str(grade_path)]
        grading = run_command(cmd, case_dir, case_dir, "original-grader",
                              timeout=120, budget_root=worker_root)
        commands.append(grading)
        if grade_path.is_file():
            outcome["grading"] = json.loads(grade_path.read_text())
            outcome["grading_receipt_sha256"] = sha256(grade_path)
        else:
            outcome["grading_error"] = "original grader did not produce a report"
    return outcome


def native_request(request: dict, label: str, case_dir: Path, worker_root: Path,
                   commands: list[dict], timeout: int) -> dict:
    driver = worker_root / "target/debug/ls-case-driver"
    if not driver.is_file():
        raise RuntimeError("native case driver binary missing")
    request_path = case_dir / f"{label}.request.json"
    receipt_path = case_dir / f"{label}.native-receipt.json"
    request_path.write_text(json.dumps(request, indent=2))
    record = run_command([str(driver), str(request_path), str(receipt_path)],
                         case_dir, case_dir, f"native-{label}", timeout=timeout,
                         budget_root=worker_root)
    commands.append(record)
    if not receipt_path.is_file():
        raise RuntimeError(f"{label} native driver lost its receipt")
    native = json.loads(receipt_path.read_text())
    native["receipt_path"] = str(receipt_path)
    native["receipt_sha256"] = sha256(receipt_path)
    return native


def verify_guest_digest(native: dict, step_name: str, expected: str) -> None:
    matches = [s for s in native.get("steps", []) if s.get("name") == step_name]
    if len(matches) != 1 or matches[0].get("exit_record") != {"kind": "EXITED", "exit_code": 0}:
        raise RuntimeError(f"{step_name} has no successful exit receipt")
    if matches[0].get("truncated"):
        raise RuntimeError(f"{step_name} output truncated")
    actual = Path(matches[0]["stdout"]).read_text().split()[0]
    if actual != expected:
        raise RuntimeError(f"{step_name} digest mismatch: {actual} != {expected}")


def extract_guest_file(native: dict, step_name: str, destination: Path) -> str:
    matches = [s for s in native.get("steps", []) if s.get("name") == step_name]
    if len(matches) != 1 or matches[0].get("exit_record") != {"kind": "EXITED", "exit_code": 0}:
        raise RuntimeError(f"guest file extraction {step_name} has no successful exit receipt")
    step = matches[0]
    if step.get("truncated"):
        raise RuntimeError(f"guest file extraction {step_name} was truncated")
    encoded = Path(step["stdout"]).read_bytes().strip()
    data = base64.b64decode(encoded, validate=True)
    destination.write_bytes(data)
    return sha256(destination)


def run_terminal(assets: Path, case_dir: Path, worker_root: Path,
                 metadata: dict, commands: list[dict]) -> dict:
    task = assets / "terminalbench/tasks/atrx-vep-crispr"
    agent_layout = case_dir / "agent-layout"
    verifier_layout = case_dir / "verifier-layout"
    metadata["agent_oci_manifest_digest"] = extract_oci_layout(case_dir / "agent.oci.tar", agent_layout)
    metadata["verifier_oci_manifest_digest"] = extract_oci_layout(case_dir / "verifier.oci.tar", verifier_layout)
    base = {"runc": str(worker_root / "bin/runc"),
            "umoci": str(worker_root / "bin/umoci"),
            "guest_init": str(worker_root / "target/x86_64-unknown-linux-musl/debug/lsguest-init"),
            "command_override": ["tail", "-f", "/dev/null"]}
    solution = task / "solution"
    agent_steps = [{"name": "solution-directory", "argv": ["mkdir", "-p", "/solution"],
                    "timeout_ms": 20_000, "require_exit_zero": True}]
    for name in ("solve.sh", "solve.py"):
        agent_steps.append(guest_upload_step(f"upload-{name}", (solution / name).read_bytes(),
                                             f"/solution/{name}"))
        agent_steps.append({"name": f"digest-{name}",
                            "argv": ["sha256sum", f"/solution/{name}"],
                            "timeout_ms": 20_000, "require_exit_zero": True})
    agent_steps.extend([
        {"name": "original-solution", "argv": ["/bin/bash", "/solution/solve.sh"],
         "timeout_ms": 28_800_000, "require_exit_zero": True},
        {"name": "artifact-digest", "argv": ["sha256sum", "/app/output/mutation.report.json"],
         "timeout_ms": 20_000, "require_exit_zero": True},
        {"name": "declared-artifact", "argv": ["base64", "-w0", "/app/output/mutation.report.json"],
         "timeout_ms": 20_000, "require_exit_zero": True},
    ])
    agent = native_request(dict(base, layout=str(agent_layout),
                                workspace=str(case_dir / "native-agent"), steps=agent_steps),
                           "agent", case_dir, worker_root, commands, timeout=29_000)
    if not agent.get("complete"):
        return {"agent": agent, "error": "agent run or cleanup incomplete"}
    for name in ("solve.sh", "solve.py"):
        verify_guest_digest(agent, f"digest-{name}", sha256(solution / name))
    artifact = case_dir / "mutation.report.json"
    artifact_sha = extract_guest_file(agent, "declared-artifact", artifact)
    verify_guest_digest(agent, "artifact-digest", artifact_sha)
    verifier_steps = [guest_upload_step("upload-declared-artifact", artifact.read_bytes(),
                                        "/app/output/mutation.report.json"),
                      {"name": "artifact-digest", "argv": ["sha256sum", "/app/output/mutation.report.json"],
                       "timeout_ms": 20_000, "require_exit_zero": True},
                      {"name": "original-verifier", "argv": ["/bin/bash", "/tests/test.sh"],
                       "timeout_ms": 600_000, "require_exit_zero": False},
                      {"name": "reward-json", "argv": ["base64", "-w0", "/logs/verifier/reward.json"],
                       "timeout_ms": 20_000, "require_exit_zero": True},
                      {"name": "ctrf-json", "argv": ["base64", "-w0", "/logs/verifier/ctrf.json"],
                       "timeout_ms": 20_000, "require_exit_zero": True}]
    verifier = native_request(dict(base, layout=str(verifier_layout),
                                   workspace=str(case_dir / "native-verifier"), steps=verifier_steps),
                              "verifier", case_dir, worker_root, commands, timeout=900)
    outcome = {"agent": agent, "verifier": verifier,
               "declared_artifact_sha256": artifact_sha}
    if not verifier.get("complete"):
        outcome["error"] = "verifier did not generate complete reward/CTRF evidence or cleanup failed"
        return outcome
    verify_guest_digest(verifier, "artifact-digest", artifact_sha)
    reward_path = case_dir / "reward.json"
    ctrf_path = case_dir / "ctrf.json"
    outcome["reward_json_sha256"] = extract_guest_file(verifier, "reward-json", reward_path)
    outcome["ctrf_json_sha256"] = extract_guest_file(verifier, "ctrf-json", ctrf_path)
    reward = json.loads(reward_path.read_text())
    ctrf = json.loads(ctrf_path.read_text())
    tests = ctrf.get("results", {}).get("summary", {}).get("tests")
    if not isinstance(tests, int) or tests <= 0:
        outcome["error"] = "original verifier collected zero or unreported tests"
        return outcome
    outcome["tests_collected"] = tests
    outcome["reward"] = reward.get("reward")
    return outcome


def run_cyber_agent(assets: Path, case_dir: Path, worker_root: Path,
                    metadata: dict, commands: list[dict]) -> dict:
    """Run the official level1 task workspace and original build in a guest.

    This exercises the agent environment only. Submission and the official
    vulnerable/fixed verifier are separate evidence states.
    """
    upstream = worker_root / "src/cybergym"
    revision = subprocess.run(["git", "-C", str(upstream), "rev-parse", "HEAD"],
                              capture_output=True, text=True)
    expected = "c6fe2027d39471375920b92cf1025e23a99ffda5"
    if revision.returncode or revision.stdout.strip() != expected:
        raise RuntimeError("CyberGym task generator source revision mismatch")
    source = upstream / "src/cybergym/task/gen_task.py"
    metadata["task_generator_sha256"] = sha256(source)
    workspace = case_dir / "official-task"
    cmd = [str(worker_root / "venv/cybergym/bin/python"), "-m", "cybergym.task.gen_task",
           "--task-id", "arvo:47101", "--agent-id", f"lsb-{case_dir.name}",
           "--out-dir", str(workspace), "--data-dir", str(assets / "cybergym/data"),
           "--server", "http://127.0.0.1:8666", "--difficulty", "level1"]
    generated = run_command(cmd, case_dir, case_dir, "official-task-generator",
                            timeout=120, budget_root=worker_root)
    commands.append(generated)
    if generated["exit_code"] != 0:
        raise RuntimeError("original CyberGym task generator failed")
    names = ("repo-vul.tar.gz", "description.txt", "README.md", "submit.sh")
    files = [workspace / name for name in names]
    if any(not path.is_file() for path in files):
        raise RuntimeError("original CyberGym task generator omitted level1 files")
    metadata["official_task_files"] = {name: sha256(path) for name, path in zip(names, files)}
    layout = case_dir / "vulnerable-layout"
    metadata["candidate_vulnerable_oci_manifest_digest"] = extract_oci_layout(
        case_dir / "source-0.oci.tar", layout)
    request = {
        "layout": str(layout), "workspace": str(case_dir / "native-agent"),
        "runc": str(worker_root / "bin/runc"),
        "umoci": str(worker_root / "bin/umoci"),
        "guest_init": str(worker_root / "target/x86_64-unknown-linux-musl/debug/lsguest-init"),
        "command_override": ["sleep", "infinity"],
        "asset_root": str(workspace),
        "file_imports": [{"source": str(path), "target": f"/workspace/{name}",
                          "sha256": sha256(path)} for name, path in zip(names, files)],
        "steps": [
            {"name": "task-workspace", "argv": ["/bin/bash", "-lc",
              "pwd; id; ls -la /workspace; sha256sum /workspace/repo-vul.tar.gz /workspace/description.txt /workspace/README.md /workspace/submit.sh"],
             "timeout_ms": 30_000, "require_exit_zero": True},
            {"name": "unpack-official-source", "argv": ["tar", "-xzf", "/workspace/repo-vul.tar.gz", "-C", "/src"],
             "timeout_ms": 180_000, "require_exit_zero": True},
            {"name": "toolchain-inspect", "argv": ["/bin/bash", "-lc",
              "printf 'CC=%s CXX=%s SANITIZER=%s SRC=%s OUT=%s\\n' \"$CC\" \"$CXX\" \"$SANITIZER\" \"$SRC\" \"$OUT\"; command -v make; command -v gcc; test -f /src/src-vul/build.sh"],
             "timeout_ms": 30_000, "require_exit_zero": True},
            {"name": "original-build", "argv": ["/bin/bash", "-lc",
              "cd /src/src-vul && bash build.sh > /workspace/build.log 2>&1"],
             "timeout_ms": 1_800_000, "require_exit_zero": False},
            {"name": "build-artifacts", "argv": ["/bin/bash", "-lc",
              "ls -la /out; stat -c '%s %n' /workspace/build.log"],
             "timeout_ms": 30_000, "require_exit_zero": False},
        ],
        "file_exports": [{"source": "/workspace/build.log", "name": "build.log",
                          "max_bytes": 64 * 1024 * 1024}],
    }
    native = native_request(request, "cyber-agent", case_dir, worker_root, commands,
                            timeout=2_400)
    outcome = {"agent": native, "generated_task_files": metadata["official_task_files"]}
    build_steps = [step for step in native.get("steps", []) if step.get("name") == "original-build"]
    outcome["original_build_exit"] = (build_steps[0].get("exit_record")
                                       if len(build_steps) == 1 else None)
    exports = [file for file in native.get("file_exports", [])
               if file.get("source") == "/workspace/build.log"]
    if len(exports) == 1 and exports[0].get("complete"):
        outcome["build_log_sha256"] = exports[0]["host_sha256"]
        outcome["build_log_path"] = exports[0]["path"]
    if not native.get("complete"):
        outcome["error"] = native.get("step_error") or native.get("error") or "agent cleanup incomplete"
    return outcome


def run(uid: str, worker: dict) -> tuple[int, dict]:
    if uid not in CASES:
        return 2, {"error": f"unsupported first-round case: {uid}"}
    root = Path(worker["workspace_root"])
    if not worker.get("authorized") or worker.get("hostname") != socket.gethostname() or worker.get("uid") != os.geteuid():
        return 3, {"error": "worker identity/authorization mismatch"}
    if not root.is_dir() or ROOT.resolve() != (root / "repo").resolve():
        return 3, {"error": "runner is outside registered worker workspace"}
    report_path = root / "hostreport.json"
    if not report_path.is_file() or sha256(report_path) != worker["hostreport_sha256"]:
        return 3, {"error": "HostReport absent or changed"}
    current_size = worker_size(root)
    if current_size > MAX_WORKER_BYTES:
        return 3, {"error": f"worker exceeds 100 GiB cap: {current_size}"}
    benchmark, denominator = CASES[uid]
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    case_dir = root / "evidence/cases" / uid.replace(":", "_") / f"{stamp}-{os.getpid()}"
    case_dir.mkdir(parents=True, mode=0o700)
    commands: list[dict] = []
    result = {"schema": "candidate-case/1", "uid": uid, "benchmark": benchmark,
              "denominator": denominator, "worker": worker["id"],
              "candidate": source_state(), "hostreport_sha256": sha256(report_path),
              "manifest_sha256": sha256(ROOT / "specs/benchmark-manifest.jsonl"),
              "started_at": datetime.now(timezone.utc).isoformat(),
              "workspace_bytes_before": current_size, "commands": commands,
              "preparation_proxy": worker.get("build_https_proxy"),
              "tool_sha256": {name: sha256(root / "bin" / name) for name in
                              ("runc", "umoci", "buildkitd", "buildctl")
                              if (root / "bin" / name).is_file()},
              "environment_execution": "NOT_STARTED", "verification_run": "NOT_RUN",
              "positive_oracle": "NOT_RUN", "result_comparison": "REFERENCE_NOT_RUN",
              "cleanup": "NO_SANDBOX_CREATED", "case_dir": str(case_dir)}
    code = 3
    try:
        row = None
        for line in (ROOT / "specs/benchmark-manifest.jsonl").open():
            item = json.loads(line)
            if item["uid"] == uid:
                row = item
                break
        if row is None:
            raise RuntimeError("case not found in full benchmark manifest")
        result["manifest_row_sha256"] = hashlib.sha256(
            json.dumps(row, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
        metadata, paths, reason = inspect_assets(uid, root / "assets")
        result["assets"] = metadata
        result["asset_files_present"] = [str(p) for p in paths if p.is_file()]
        if reason:
            result["blocked_at"] = "assets"
            result["reason"] = reason
        else:
            result["environment_execution"] = "ASSETS_PRESENT"
            reason = build_images(uid, root / "assets", case_dir, root, metadata,
                                  commands, worker.get("build_https_proxy"))
            if reason:
                result["blocked_at"] = "image_preparation"
                result["reason"] = reason
            else:
                result["environment_execution"] = "IMAGES_PREPARED"
                if benchmark == "swebench":
                    native = run_swe(uid, root / "assets", case_dir, root, metadata, commands)
                    result["native"] = native
                    gold = native["gold"]
                    wrong = native["wrong_patch"]
                    if gold.get("steps"):
                        result["environment_execution"] = "COMMANDS_EXECUTED"
                    if gold.get("cleanup", {}).get("all_ok") and wrong.get("cleanup", {}).get("all_ok"):
                        result["cleanup"] = "NATIVE_CLEANUP_RECEIPTS_PRESENT"
                    grade = native.get("grading")
                    if grade and grade.get("test_output_found") and grade.get("parsed_test_count", 0) > 0:
                        result["verification_run"] = "RAN"
                        resolved = grade["upstream_report"]["astropy__astropy-11693"]["resolved"]
                        result["positive_oracle"] = (
                            "CANDIDATE_ONLY_POSITIVE" if resolved else "CANDIDATE_ONLY_NEGATIVE")
                    if not gold.get("complete"):
                        result["blocked_at"] = "gold_execution"
                        result["reason"] = gold.get("step_error") or gold.get("error") or "gold native run incomplete"
                    elif not native["wrong_patch_rejected"]:
                        result["blocked_at"] = "negative_control"
                        result["reason"] = "malformed patch was not rejected with complete cleanup"
                    elif result["verification_run"] != "RAN":
                        result["blocked_at"] = "oracle_grading"
                        result["reason"] = native.get("grading_error") or "original grader did not parse tests"
                    elif result["positive_oracle"] == "CANDIDATE_ONLY_POSITIVE":
                        result["blocked_at"] = "reference_comparison"
                        result["reason"] = "candidate-only positive; reference not run"
                        code = 0
                    else:
                        result["blocked_at"] = "positive_oracle"
                        result["reason"] = "gold patch unresolved under original grader"
                        code = 1
                elif benchmark == "terminalbench":
                    native = run_terminal(root / "assets", case_dir, root, metadata, commands)
                    result["native"] = native
                    result["environment_execution"] = "COMMANDS_EXECUTED" if native.get("agent", {}).get("steps") else "IMAGES_PREPARED"
                    if any(s.get("name") == "original-verifier" for s in native.get("verifier", {}).get("steps", [])):
                        result["verification_run"] = "RAN"
                    if native.get("agent", {}).get("cleanup", {}).get("all_ok") and native.get("verifier", {}).get("cleanup", {}).get("all_ok"):
                        result["cleanup"] = "NATIVE_CLEANUP_RECEIPTS_PRESENT"
                    if native.get("reward") == 1 and native.get("tests_collected", 0) > 0:
                        result["positive_oracle"] = "CANDIDATE_ONLY_POSITIVE"
                    result["blocked_at"] = "reference_comparison" if result["positive_oracle"] == "CANDIDATE_ONLY_POSITIVE" else "verifier_or_execution"
                    result["reason"] = native.get("error") or "candidate-only result; reference not run"
                else:
                    native = run_cyber_agent(root / "assets", case_dir, root, metadata, commands)
                    result["native"] = native
                    agent = native["agent"]
                    if agent.get("steps"):
                        result["environment_execution"] = "COMMANDS_EXECUTED"
                    if agent.get("cleanup", {}).get("all_ok") and not agent.get("live_after"):
                        result["cleanup"] = "NATIVE_CLEANUP_RECEIPTS_PRESENT"
                    result["blocked_at"] = "candidate_verification"
                    result["reason"] = native.get("error") or (
                        "official agent workspace and build ran; invalid PoC submission and "
                        "original vulnerable/fixed verifier not yet connected")
                    if not agent.get("complete") or native.get("original_build_exit") != {"kind": "EXITED", "exit_code": 0}:
                        code = 1
        result["workspace_bytes_after"] = worker_size(root)
        if result["workspace_bytes_after"] > MAX_WORKER_BYTES:
            result["blocked_at"] = "budget"
            result["reason"] = "worker exceeded 100 GiB cap"
            code = 1
    except Exception as exc:
        result["blocked_at"] = "runner"
        result["reason"] = repr(exc)
        code = 1
    result["finished_at"] = datetime.now(timezone.utc).isoformat()
    result["exit_code"] = code
    (case_dir / "receipt.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return code, result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--task", required=True)
    parser.add_argument("--worker", required=True)
    args = parser.parse_args()
    registry = json.loads((ROOT / "dev/runtime-workers.json").read_text())
    worker = registry.get(args.worker)
    if not worker:
        print(json.dumps({"error": "worker not registered"}))
        return 3
    worker = dict(worker, id=args.worker)
    code, result = run(args.task, worker)
    native = result.get("native", {})
    grading = native.get("grading", {})
    count = grading.get("raw_reported_total") or grading.get("parsed_test_count") or native.get("tests_collected", 0)
    print(json.dumps({"exit_code": code, "case_dir": result.get("case_dir"),
                      "blocked_at": result.get("blocked_at"), "reason": result.get("reason"),
                      "tests_collected": count, "denominator": result.get("denominator"),
                      "environment_execution": result.get("environment_execution"),
                      "verification_run": result.get("verification_run"),
                      "positive_oracle": result.get("positive_oracle"),
                      "result_comparison": result.get("result_comparison"),
                      "cleanup": result.get("cleanup")}))
    return code


if __name__ == "__main__":
    sys.exit(main())
