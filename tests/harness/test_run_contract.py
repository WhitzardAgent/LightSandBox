"""T00 runner contract tests: unknown suites fail, child exit codes propagate,
missing tools are explicit prerequisites, zero collected tests is a failure.

These tests exercise dev/check as a real subprocess and checklib as a module.
They never touch runtime-level resources.
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEV_CHECK = ROOT / 'dev' / 'check'
sys.path.insert(0, str(ROOT / 'dev'))

import checklib  # noqa: E402

FIXTURES = Path(__file__).resolve().parent / 'fixtures'


class UnknownSuiteTests(unittest.TestCase):
    """T00-C01: unknown suite or empty collection must exit non-zero."""

    def test_unknown_suite_fails_with_listing(self):
        r = subprocess.run([sys.executable, str(DEV_CHECK), 'not-a-suite'],
                           capture_output=True, text=True, timeout=30)
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        out = (r.stdout + r.stderr).lower()
        self.assertIn('unknown suite', out)
        self.assertIn('unit', out)  # usage lists the real suites

    def test_no_arguments_is_usage_error(self):
        r = subprocess.run([sys.executable, str(DEV_CHECK)],
                           capture_output=True, text=True, timeout=30)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn('usage', (r.stdout + r.stderr).lower())

    def test_zero_collected_tests_fails(self):
        with tempfile.TemporaryDirectory() as td:
            res = checklib.python_unittest(Path(td), Path(td))
        self.assertEqual(res['tests'], 0)
        self.assertNotEqual(res['exit'], 0)
        self.assertIn('zero tests', res.get('reason', ''))


class ExitPropagationTests(unittest.TestCase):
    """T00-C02: a child exiting 17 must surface as exit 17, never as PASS."""

    def test_command_exit_code_is_preserved(self):
        res = checklib.run_command([sys.executable, str(FIXTURES / 'exit17.py')])
        self.assertEqual(res['exit'], 17)
        self.assertIn('17', res['output'])

    def test_exit17_selftest_suite_is_red(self):
        summary = checklib.run_suite('selftest-exit17', ROOT)
        self.assertNotEqual(summary['exit'], 0, summary)
        self.assertEqual(summary['commands'][0]['exit'], 17)
        self.assertGreaterEqual(summary['failures'], 1)


class PrerequisiteTests(unittest.TestCase):
    def test_missing_cargo_is_blocked_prereq_not_pass(self):
        env = dict(os.environ)
        env['PATH'] = ''
        res = checklib.cargo_test(ROOT, env=env)
        self.assertEqual(res['exit'], checklib.EXIT_PREREQ, res)
        self.assertIn('cargo', res.get('reason', ''))

    def test_runtime_suite_requires_registered_worker(self):
        summary = checklib.run_suite('runtime', ROOT, task='T00', worker='WORKER_DOES_NOT_EXIST')
        self.assertEqual(summary['exit'], checklib.EXIT_BLOCKED, summary)
        self.assertIn('worker', summary.get('reason', ''))

    def test_runtime_suite_requires_worker_flag(self):
        summary = checklib.run_suite('runtime', ROOT, task='T00', worker=None)
        self.assertEqual(summary['exit'], checklib.EXIT_BLOCKED, summary)


if __name__ == '__main__':
    unittest.main()
