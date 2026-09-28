import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent


class TestPythonExample(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.proc = subprocess.run([sys.executable, str(HERE / 'example.py')], cwd=str(ROOT),
                                  capture_output=True, text=True, timeout=120)

    def test_exits_zero(self):
        self.assertEqual(self.proc.returncode, 0, self.proc.stderr)

    def test_typed_surface_and_resolution(self):
        out = self.proc.stdout
        self.assertIn('"sandbox_id": "sb_demo"', out)
        self.assertIn('"kind": "argv"', out)
        self.assertIn('ExitStatus normalized exit code: 0', out)
        self.assertIn('"mode": "image"', out)
        self.assertIn('["/.lightsandbox/bin/init", "--idle"]', out)
        self.assertIn('rejected as expected', out)

    def test_plan_is_offline_and_deterministic(self):
        out = self.proc.stdout
        self.assertIn('"execution_ready": false', out)
        self.assertIn('"executed": false', out)
        self.assertIn('"plan_digest": "sha256:', out)
        self.assertIn('planning-only', out)
        again = subprocess.run([sys.executable, str(HERE / 'example.py')], cwd=str(ROOT),
                               capture_output=True, text=True, timeout=120)
        self.assertEqual(self.proc.stdout, again.stdout, 'example output must be deterministic')

    def test_environment_file_passes_offline_validation(self):
        sys.path.insert(0, str(ROOT / 'client' / 'python' / 'src'))
        from lightsandbox.contracts import validate_environment
        env = json.loads((HERE / 'environment.json').read_text(encoding='utf-8'))
        checked = validate_environment(env)
        self.assertEqual(sorted(checked['services']), ['main'])
        self.assertIn('sha256:', checked['services']['main']['image'])


if __name__ == '__main__':
    unittest.main()
