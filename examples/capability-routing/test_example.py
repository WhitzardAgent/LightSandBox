import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent


class TestCapabilityRoutingExample(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.proc = subprocess.run([sys.executable, str(HERE / 'route.py')], cwd=str(ROOT),
                                  capture_output=True, text=True, timeout=60)

    def test_exits_zero(self):
        self.assertEqual(self.proc.returncode, 0, self.proc.stderr)

    def test_empty_requirements_select_native(self):
        self.assertIn('plan: NATIVE (lightest available backend satisfying all '
                      '0 required dimension(s))', self.proc.stdout)
        self.assertIn('AUTO order: NATIVE -> KERNEL -> MACHINE', self.proc.stdout)

    def test_multi_uid_routes_to_kernel_with_citations(self):
        out = self.proc.stdout
        self.assertIn('rule: multi_uid -> host capability identity.uid_mapping_mode MUST be '
                      'MULTI_ID  [contracts/v1/resolution-rules.json requirements_map.multi_uid]',
                      out)
        self.assertIn('NATIVE: observed SINGLE_ID', out)
        self.assertIn('HOST_CAPABILITY_UNAVAILABLE / HTTP 412', out)
        self.assertIn('KERNEL: observed MULTI_ID', out)
        self.assertIn('plan: KERNEL (lightest available backend satisfying all '
                      '1 required dimension(s))', out)
        self.assertIn('planning-level simulation only', out)

    def test_simulation_breaks_loudly_on_evidence_drift(self):
        probe = subprocess.run(
            [sys.executable, '-c',
             'import json, runpy, sys; p = "evidence/host/hostreport-v1.json";'
             'src = json.load(open(p));'
             'src["findings"] = [f for f in src["findings"] if f["name"] != "uid_mappings"];'
             'json.dump(src, open("/tmp/lsbx-drift-hostreport.json", "w"));'
             'sys.path.insert(0, "examples/capability-routing");'
             'import route; route.HOST_REPORT = route.Path("/tmp/lsbx-drift-hostreport.json");'
             'route.main()'],
            cwd=str(ROOT), capture_output=True, text=True, timeout=60)
        self.assertNotEqual(probe.returncode, 0, probe.stdout + probe.stderr)
        self.assertIn('drift', probe.stderr)


if __name__ == '__main__':
    unittest.main()
