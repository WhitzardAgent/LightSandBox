import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent


class TestCodingWorkspaceExample(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.proc = subprocess.run([sys.executable, str(HERE / 'stage_workload.py')],
                                  cwd=str(ROOT), capture_output=True, text=True, timeout=120)

    def test_exits_zero(self):
        self.assertEqual(self.proc.returncode, 0, self.proc.stderr)

    def test_plan_and_preview_are_not_executed(self):
        out = self.proc.stdout
        self.assertIn("workspace fixture (failing test by design): ['calc.py', 'test_calc.py']", out)
        self.assertIn('"execution_ready": false', out)
        self.assertGreaterEqual(out.count('"executed": false'), 2, out)
        self.assertNotIn('"executed": true', out)
        self.assertIn('runtime worker', out)
        line = [l for l in out.splitlines()
                if l.startswith('{') and '"method": "POST"' in l][0]
        request = json.loads(line)
        self.assertEqual(request['path'], '/v1/sandboxes/sb_workspace/executions')
        self.assertEqual(request['body']['command']['argv'], ['python', '-m', 'pytest', '-q'])
        self.assertEqual(request['body']['cwd'], '/workspace')

    def test_fixture_test_actually_fails_as_designed(self):
        probe = subprocess.run([sys.executable, '-m', 'unittest', 'test_calc', '-v'],
                               cwd=str(HERE / 'workspace'), capture_output=True, text=True,
                               timeout=60)
        self.assertEqual(probe.returncode, 1, probe.stdout + probe.stderr)
        self.assertIn('FAILED', probe.stderr)

    def test_fixture_is_git_less(self):
        self.assertFalse((HERE / 'workspace' / '.git').exists())


if __name__ == '__main__':
    unittest.main()
