"""T03 contract tests: the formal HostReport is machine-readable, binds every
required capability to a real evidence command + exit code + output excerpt,
and never upgrades a degraded host fact into a product claim.

The report is produced by lightsandbox_dev.host_probe probing the *current*
host with read-only argv probes (re-runnable), then validated by the shared
capability_report rule set mirrored from ls-core.
"""
import hashlib
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python" / "src"))

from lightsandbox_dev.capability_report import validate_report  # noqa: E402

REPORT_PATH = ROOT / "evidence" / "host" / "hostreport-v1.json"
PROFILES_PATH = ROOT / "specs" / "host-profiles.json"

REQUIRED_CAPABILITIES = (
    "userns", "uid_mappings", "cgroup", "procfs", "mount", "rootfs",
    "network", "seccomp", "lsm", "filesystem", "resource",
)
GOVERNED_STATUSES = {"SUPPORTED", "DEGRADED", "UNSUPPORTED", "NOT_TESTED"}


def load_report() -> dict:
    return json.loads(REPORT_PATH.read_text(encoding="utf-8"))


class HostReportFileTests(unittest.TestCase):
    def test_report_exists_and_is_structurally_valid(self):
        self.assertTrue(REPORT_PATH.is_file(), "hostreport-v1.json missing; run the T03 probe")
        errors = validate_report(load_report())
        self.assertEqual(errors, [], errors)

    def test_all_required_capabilities_present(self):
        report = load_report()
        names = {f["name"] for f in report["findings"]}
        missing = [c for c in REQUIRED_CAPABILITIES if c not in names]
        self.assertEqual(missing, [], f"missing capability findings: {missing}")

    def test_every_finding_binds_real_evidence(self):
        for finding in load_report()["findings"]:
            with self.subTest(finding=finding["name"]):
                self.assertIn(finding["status"], GOVERNED_STATUSES)
                command = finding["evidence_command"]
                self.assertTrue(command.strip(), "evidence command must not be empty")
                self.assertFalse(
                    command.lstrip().startswith(("sh ", "bash ", "sh -c", "bash -c")),
                    "shell composition is not an admissible evidence command")
                self.assertIsInstance(finding.get("exit_code"), int,
                                      "evidence must carry the measured exit code")
                self.assertTrue(finding.get("output_excerpt", "").strip(),
                                "important output excerpt must be recorded")

    def test_unsupported_and_degraded_findings_carry_measured_failure(self):
        for finding in load_report()["findings"]:
            if finding["status"] in ("UNSUPPORTED", "DEGRADED"):
                with self.subTest(finding=finding["name"]):
                    self.assertNotEqual(finding["exit_code"], 0,
                                        "degraded/unsupported must cite the failing measurement")
                    self.assertTrue(finding.get("limitation"),
                                    "degraded/unsupported must state the limitation")

    def test_no_capability_claims_hard_cgroup_or_full_uid_parity(self):
        report = load_report()
        findings = {f["name"]: f for f in report["findings"]}
        for guarded in ("cgroup", "uid_mappings", "resource"):
            with self.subTest(guarded=guarded):
                self.assertNotEqual(findings[guarded]["status"], "SUPPORTED",
                                    f"{guarded} must not be claimed SUPPORTED on this host")

    def test_derived_tier_is_portable_rootless(self):
        report = load_report()
        self.assertEqual(report["derived_tier"], "PORTABLE_ROOTLESS")
        self.assertIn("PORTABLE_ROOTLESS",
                      json.dumps(report.get("tier_notes", {})))

    def test_probe_repeatable_and_status_stable(self):
        from lightsandbox_dev import host_probe
        first = host_probe.build_report()
        second = host_probe.build_report()
        self.assertEqual(validate_report(first), [])
        status_first = {f["name"]: f["status"] for f in first["findings"]}
        status_second = {f["name"]: f["status"] for f in second["findings"]}
        self.assertEqual(status_first, status_second)
        stored = {f["name"]: f["status"] for f in load_report()["findings"]}
        self.assertEqual(status_first, stored, "stored report must match live host probe")


class HostProfilesIntegrationTests(unittest.TestCase):
    def test_portable_rootless_profile_observed_and_bound(self):
        profiles = json.loads(PROFILES_PATH.read_text(encoding="utf-8"))
        portable = next((p for p in profiles["profiles"] if p["id"] == "portable-rootless"), None)
        self.assertIsNotNone(portable, "specs/host-profiles.json lacks the portable-rootless profile")
        self.assertTrue(portable.get("observed"))
        rel = portable.get("report")
        self.assertTrue(rel, "profile must reference the HostReport path")
        target = ROOT / rel
        self.assertTrue(target.is_file(), rel)
        digest = hashlib.sha256(target.read_bytes()).hexdigest()
        self.assertEqual(portable.get("report_sha256"), digest,
                         "profile report hash must match the committed report bytes")

    def test_existing_profiles_untouched(self):
        profiles = json.loads(PROFILES_PATH.read_text(encoding="utf-8"))
        ids = [p["id"] for p in profiles["profiles"]]
        self.assertIn("vm-rootful-parity", ids)
        self.assertIn("vm-rootless-tested", ids)
        self.assertIn("development-no-runtime", ids)


if __name__ == "__main__":
    unittest.main()
