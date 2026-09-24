"""T01 contract tests: the upstream source lock carries immutable revisions,
tree hashes, license bytes and roles; the release gate refuses partial locks.

Synthetic clones exercise tool behavior; the real catalog is validated for
internal completeness (never against the network).
"""
from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "tools" / "lock_local_sources.py"
CATALOG = ROOT / "specs" / "upstream-lock.json"
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")

EXPECTED_ROLES = {
    "runc": "oci-runtime",
    "umoci": "image-unpack",
    "runtime-spec": "spec",
    "image-spec": "spec",
    "proot": "compat-backend",
    "opensandbox": "api-reference",
    "swebench": "benchmark",
    "terminalbench": "benchmark",
    "cybergym": "benchmark",
    "harbor": "adapter-reference",
    "buildkit": "oci-worker",
}
ALLOWED_SPDX = {"Apache-2.0", "MIT", "GPL-2.0-only", "GPL-2.0-or-later"}


def run_tool(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([sys.executable, str(TOOL), *args],
                          capture_output=True, text=True, timeout=60)


class SyntheticLockTests(unittest.TestCase):
    """Behavior of tools/lock_local_sources.py on a minimal synthetic catalog."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        td = Path(self.tmp.name)
        self.repos = {}
        for name, files in (("runc", {"README.md": b"fixture readme\n", "LICENSE": b"Apache License\nfixture\n"}),
                            ("umoci", {"README.md": b"umoci fixture\n", "COPYING": b"Apache License\nfixture\n"})):
            repo = td / name
            repo.mkdir()
            self.git(repo, "init", "-q")
            self.git(repo, "remote", "add", "origin", f"https://github.com/opencontainers/{name}.git")
            for rel, data in files.items():
                (repo / rel).write_bytes(data)
            self.git(repo, "add", ".")
            self.git(repo, "-c", "user.name=F", "-c", "user.email=f@example.invalid", "commit", "-qm", "fixture")
            self.repos[name] = repo
        self.catalog = td / "catalog.json"
        self.catalog.write_text(json.dumps({
            "schema_version": "2.0", "status": "UNLOCKED",
            "repositories": [
                {"name": "runc", "url": "https://github.com/opencontainers/runc.git",
                 "revision": None, "status": "UNLOCKED",
                 "role": "oci-runtime", "spdx": "Apache-2.0",
                 "required_files": ["README.md"]},
                {"name": "umoci", "url": "https://github.com/opencontainers/umoci.git",
                 "revision": None, "status": "UNLOCKED",
                 "role": "image-unpack", "spdx": "Apache-2.0",
                 "required_files": ["README.md"]},
            ]}))

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, repo: Path, *args: str) -> str:
        r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, timeout=10)
        self.assertEqual(r.returncode, 0, r.stderr)
        return r.stdout.strip()

    def lock_all(self, *extra: str) -> subprocess.CompletedProcess:
        return run_tool("--catalog", str(self.catalog),
                        "--repo", f"runc={self.repos['runc']}",
                        "--repo", f"umoci={self.repos['umoci']}", *extra)

    def test_tree_hash_recorded(self):
        r = self.lock_all()
        self.assertEqual(r.returncode, 0, r.stderr)
        report = json.loads(r.stdout)
        item = next(x for x in report["repositories"] if x["name"] == "runc")
        self.assertEqual(item["tree_hash"], self.git(self.repos["runc"], "rev-parse", "HEAD^{tree}"))

    def test_license_bytes_recorded(self):
        r = self.lock_all()
        self.assertEqual(r.returncode, 0, r.stderr)
        report = json.loads(r.stdout)
        item = next(x for x in report["repositories"] if x["name"] == "runc")
        expect = hashlib.sha256((self.repos["runc"] / "LICENSE").read_bytes()).hexdigest()
        self.assertEqual(item["license"]["path"], "LICENSE")
        self.assertEqual(item["license"]["sha256"], expect)
        umoci = next(x for x in report["repositories"] if x["name"] == "umoci")
        self.assertEqual(umoci["license"]["path"], "COPYING")

    def test_role_and_spdx_propagate(self):
        r = self.lock_all()
        self.assertEqual(r.returncode, 0, r.stderr)
        report = json.loads(r.stdout)
        item = next(x for x in report["repositories"] if x["name"] == "runc")
        self.assertEqual(item["role"], "oci-runtime")
        self.assertEqual(item["spdx"], "Apache-2.0")

    def test_check_release_accepts_full_lock(self):
        r = self.lock_all("--output", str(Path(self.tmp.name) / "locked.json"))
        self.assertEqual(r.returncode, 0, r.stderr)
        gate = run_tool("--catalog", str(Path(self.tmp.name) / "locked.json"), "--check-release")
        self.assertEqual(gate.returncode, 0, gate.stderr + gate.stdout)

    def test_check_release_rejects_partial_lock(self):
        gate = run_tool("--catalog", str(self.catalog), "--check-release")
        self.assertNotEqual(gate.returncode, 0)
        out = gate.stderr + gate.stdout
        self.assertIn("runc", out)
        self.assertIn("umoci", out)

    def test_tool_rejects_short_revision_in_check_release(self):
        bad = json.loads(self.catalog.read_text())
        bad["status"] = "LOCKED"
        bad["repositories"][0].update(status="LOCKED", revision="41b7477",
                                      file_hashes={"README.md": "0" * 64})
        bad["repositories"][1].update(status="LOCKED", revision="f" * 40,
                                      tree_hash="e" * 40,
                                      license={"path": "COPYING", "sha256": "d" * 64},
                                      file_hashes={"README.md": "0" * 64})
        path = Path(self.tmp.name) / "bad.json"
        path.write_text(json.dumps(bad))
        gate = run_tool("--catalog", str(path), "--check-release")
        self.assertNotEqual(gate.returncode, 0)
        self.assertIn("revision", gate.stderr + gate.stdout)

    # ---- round-01 review finding #5: port the verifier's negative probes ----

    def _locked_catalog(self) -> Path:
        r = self.lock_all("--output", str(Path(self.tmp.name) / "locked.json"))
        self.assertEqual(r.returncode, 0, r.stderr)
        return Path(self.tmp.name) / "locked.json"

    def test_lock_rejects_dirty_clone(self):
        (self.repos["runc"] / "README.md").write_bytes(b"dirtied\n")
        r = self.lock_all()
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("dirty", (r.stderr + r.stdout).lower())

    def test_lock_rejects_missing_required_file(self):
        bad = json.loads(self.catalog.read_text())
        bad["repositories"][0]["required_files"] = ["NO_SUCH_FILE.md"]
        path = Path(self.tmp.name) / "missing.json"
        path.write_text(json.dumps(bad))
        r = run_tool("--catalog", str(path),
                     "--repo", f"runc={self.repos['runc']}",
                     "--repo", f"umoci={self.repos['umoci']}")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("NO_SUCH_FILE.md", r.stderr + r.stdout)

    def test_lock_rejects_origin_mismatch(self):
        bad = json.loads(self.catalog.read_text())
        bad["repositories"][0]["url"] = "https://github.com/opencontainers/attacker.git"
        path = Path(self.tmp.name) / "mismatch.json"
        path.write_text(json.dumps(bad))
        r = run_tool("--catalog", str(path),
                     "--repo", f"runc={self.repos['runc']}",
                     "--repo", f"umoci={self.repos['umoci']}")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("origin", (r.stderr + r.stdout).lower())

    def test_gate_rejects_invalid_file_hash(self):
        locked = json.loads(self._locked_catalog().read_text())
        first = locked["repositories"][0]
        rel = next(iter(first["file_hashes"]))
        first["file_hashes"][rel] = "zz" * 32
        path = Path(self.tmp.name) / "badhash.json"
        path.write_text(json.dumps(locked))
        gate = run_tool("--catalog", str(path), "--check-release")
        self.assertNotEqual(gate.returncode, 0)
        self.assertIn("sha256", (gate.stderr + gate.stdout).lower())

    def test_gate_rejects_missing_license_bytes(self):
        locked = json.loads(self._locked_catalog().read_text())
        locked["repositories"][0]["license"] = {"path": "LICENSE"}
        path = Path(self.tmp.name) / "nolicense.json"
        path.write_text(json.dumps(locked))
        gate = run_tool("--catalog", str(path), "--check-release")
        self.assertNotEqual(gate.returncode, 0)
        self.assertIn("license", (gate.stderr + gate.stdout).lower())

    def test_gate_rejects_empty_role(self):
        locked = json.loads(self._locked_catalog().read_text())
        locked["repositories"][0]["role"] = ""
        path = Path(self.tmp.name) / "norole.json"
        path.write_text(json.dumps(locked))
        gate = run_tool("--catalog", str(path), "--check-release")
        self.assertNotEqual(gate.returncode, 0)
        self.assertIn("role", (gate.stderr + gate.stdout).lower())


class RealCatalogTests(unittest.TestCase):
    """The shipped catalog must be internally complete; no network access."""

    def setUp(self):
        self.data = json.loads(CATALOG.read_text(encoding="utf-8"))

    def test_fully_locked(self):
        problems = []
        for name in ("runc", "umoci", "runtime-spec", "image-spec", "proot"):
            item = next((x for x in self.data["repositories"] if x["name"] == name), None)
            if item is None:
                problems.append(f"{name}: not registered")
                continue
            if item.get("status") != "LOCKED":
                problems.append(f"{name}: {item.get('status')}")
        self.assertEqual(self.data.get("status"), "LOCKED", problems)
        self.assertEqual(problems, [], problems)

    def test_immutable_fields_complete(self):
        for item in self.data["repositories"]:
            name = item["name"]
            with self.subTest(repo=name):
                self.assertTrue(HEX40.fullmatch(str(item.get("revision", ""))), name)
                self.assertTrue(HEX40.fullmatch(str(item.get("tree_hash", ""))), name)
                lic = item.get("license") or {}
                self.assertTrue(lic.get("path"), name)
                self.assertTrue(HEX64.fullmatch(str(lic.get("sha256", ""))), name)
                self.assertTrue(item.get("role"), name)
                self.assertIn(item.get("spdx"), ALLOWED_SPDX, name)
                for rel, digest in (item.get("file_hashes") or {}).items():
                    self.assertTrue(HEX64.fullmatch(str(digest)), f"{name}:{rel}")

    def test_roles_match_round_contract(self):
        roles = {x["name"]: x.get("role") for x in self.data["repositories"]}
        for name, role in EXPECTED_ROLES.items():
            self.assertEqual(roles.get(name), role, name)

    def test_urls_are_canonical_upstreams(self):
        for item in self.data["repositories"]:
            self.assertRegex(item["url"], r"^https://github\.com/", item["name"])
            self.assertTrue(item["url"].endswith(".git"), item["name"])


if __name__ == "__main__":
    unittest.main()
