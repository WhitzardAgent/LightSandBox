"""Round-04 §4 regression: `dev/check` results must not depend on the caller's
shell environment.

Root causes pinned before this fix (evidence: progress/2026-09-27-round-04.md
§4 notes): bare `python3` = 3.10 lacks tomllib and has different
unittest/asyncio semantics (72 collected + 2 errors where 3.12 collects 76 and
passes); checklib children inherited raw caller env (PYTHONPATH/locale bleed)
and caller cwd. The fix whitelists child env (child_env()), pins cwd to the
repo root, and re-execs old interpreters. This test locks all three laws.
"""
from __future__ import annotations

import json
import os
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CHECK = ROOT / 'dev' / 'check'
TIMEOUT_S = 1200


def run_check(env, cwd, suite='unit'):
    """Run `dev/check <suite> --json`; return (exit_code, summary_dict)."""
    proc = subprocess.run(
        [str(CHECK), suite, '--json'],
        env=env, cwd=str(cwd), capture_output=True, text=True, timeout=TIMEOUT_S,
    )
    summary = None
    for line in reversed(proc.stdout.splitlines()):
        line = line.strip()
        if line.startswith('{'):
            summary = json.loads(line)
            break
    return proc.returncode, summary, (proc.stdout + proc.stderr)


class EnvDeterminismTest(unittest.TestCase):
    def test_baseline_unit_suite_green(self):
        """Normal env, repo-root cwd: parseable summary, exit 0, tests > 0."""
        code, summary, out = run_check(env=None, cwd=ROOT)
        self.assertEqual(code, 0, out[-2000:])
        self.assertIsNotNone(summary, 'no JSON summary emitted')
        self.assertGreater(summary['tests_total'], 0)

    def test_empty_environment_matches_normal(self):
        """`env -i dev/check unit` must collect and pass exactly the same."""
        code, normal, out_n = run_check(env=None, cwd=ROOT)
        self.assertEqual(code, 0, out_n[-2000:])
        code_clean, clean, out_c = run_check(env={}, cwd=ROOT)
        self.assertEqual(code_clean, 0, out_c[-2000:])
        self.assertIsNotNone(clean, 'no JSON summary under empty env')
        self.assertEqual(normal['tests_total'], clean['tests_total'],
                         f"collection drift empty-env vs normal: "
                         f"{normal['tests_total']} vs {clean['tests_total']}")
        self.assertEqual(normal['failures'], clean['failures'])

    def test_rogue_env_and_foreign_cwd_do_not_leak(self):
        """Hostile PYTHONPATH/locale + cwd=/tmp must not change the result."""
        rogue = {
            'PATH': os.environ.get('PATH') or os.defpath,
            'HOME': os.environ.get('HOME', '/root'),
            'PYTHONPATH': '/nonexistent-rogue-lsbx-9f3d:/also/not/here',
            'PYTHONSTARTUP': '/nonexistent-rogue-lsbx-startup.py',
            'LC_ALL': 'C',
            'LANG': 'C',
        }
        code, normal, out_n = run_check(env=None, cwd=ROOT)
        self.assertEqual(code, 0, out_n[-2000:])
        code_rogue, summary, out_r = run_check(env=rogue, cwd=Path('/tmp'))
        self.assertEqual(code_rogue, 0, out_r[-2000:])
        self.assertIsNotNone(summary, 'no JSON summary under rogue env')
        self.assertEqual(normal['tests_total'], summary['tests_total'],
                         f"collection drift rogue-env vs normal: "
                         f"{normal['tests_total']} vs {summary['tests_total']}")
        self.assertEqual(normal['failures'], summary['failures'])


if __name__ == '__main__':
    unittest.main()
