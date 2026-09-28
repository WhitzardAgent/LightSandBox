import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
IMAGE = 'registry.example.invalid/demo@sha256:' + 'a' * 64


class TestHelloExample(unittest.TestCase):
    def run_example(self, argv):
        return subprocess.run(argv, cwd=str(ROOT), capture_output=True, text=True, timeout=120)

    def test_main_exits_zero(self):
        proc = self.run_example([sys.executable, str(HERE / 'main.py')])
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_output_is_planning_only(self):
        proc = self.run_example([sys.executable, str(HERE / 'main.py')])
        out = proc.stdout
        self.assertIn('"client_version": "0.5.0.dev0"', out)
        self.assertIn('"runtime_implemented": false', out)
        self.assertIn('"argv": ["echo", "hello"]', out)
        self.assertIn(IMAGE, out)
        self.assertGreaterEqual(out.count('"executed": false'), 2, out)
        self.assertNotIn('"executed": true', out)
        self.assertIn('planning-only', out)

    def test_run_sh_entry_point(self):
        proc = self.run_example(['bash', str(HERE / 'run.sh')])
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn('planning-only', proc.stdout)

    def test_dry_run_shape_is_the_documented_request(self):
        proc = self.run_example([sys.executable, str(HERE / 'main.py')])
        line = [l for l in proc.stdout.splitlines()
                if l.startswith('{') and '"method": "POST"' in l and 'executions' in l][0]
        request = json.loads(line)
        self.assertEqual(request['path'], '/v1/sandboxes/sb_demo/executions')
        self.assertEqual(request['body']['command'], {'kind': 'argv', 'argv': ['echo', 'hello']})
        self.assertEqual(request['idempotency_key'], 'dry-run-key')
        self.assertFalse(request['executed'])


if __name__ == '__main__':
    unittest.main()
