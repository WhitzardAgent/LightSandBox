"""Tests for the Python mirror of the HostCapabilityReport contract (T00)."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'src'))

from lightsandbox_dev.capability_report import validate_finding, validate_report  # noqa: E402


def good_report():
    return {
        "probe_date": "2026-09-24",
        "hostname": "fixture-host",
        "findings": [
            {"name": "userns.single_id_mapping", "status": "SUPPORTED",
             "evidence_command": "unshare -Ur true", "exit_code": 0,
             "output_excerpt": ""},
            {"name": "cgdegrp.delegation".replace("gdegrp", "group"), "status": "NOT_TESTED",
             "evidence_command": "read /sys/fs/cgroup", "exit_code": None,
             "output_excerpt": "deferred"},
        ],
        "derived_tier": "PORTABLE_ROOTLESS",
    }


class ReportValidationTests(unittest.TestCase):
    def test_well_formed_report_passes(self):
        self.assertEqual(validate_report(good_report()), [])

    def test_unknown_status_rejected(self):
        report = good_report()
        report["findings"][0]["status"] = "WORKS"
        errors = validate_report(report)
        self.assertTrue(any("status" in e for e in errors), errors)

    def test_supported_requires_zero_or_absent_exit_code(self):
        report = good_report()
        report["findings"][0]["exit_code"] = 13
        errors = validate_report(report)
        self.assertTrue(any("SUPPORTED" in e for e in errors), errors)

    def test_finding_must_bind_evidence_command(self):
        report = good_report()
        report["findings"][0]["evidence_command"] = "  "
        errors = validate_report(report)
        self.assertTrue(any("evidence_command" in e for e in errors), errors)

    def test_duplicate_finding_names_rejected(self):
        report = good_report()
        report["findings"].append(dict(report["findings"][0]))
        errors = validate_report(report)
        self.assertTrue(any("duplicate" in e for e in errors), errors)

    def test_unknown_tier_rejected(self):
        report = good_report()
        report["derived_tier"] = "KIND_OF_ROOTLESS"
        errors = validate_report(report)
        self.assertTrue(any("derived_tier" in e for e in errors), errors)

    def test_missing_top_level_keys_rejected(self):
        self.assertTrue(validate_report({}))
        self.assertTrue(validate_finding({}))


if __name__ == '__main__':
    unittest.main()
