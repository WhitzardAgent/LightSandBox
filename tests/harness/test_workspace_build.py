"""T00-C04: in a clean temporary tree the workspace must build and the
runner must collect a positive number of tests — with no production
completeness claim. This is deliberately slow (real cargo build) and is
only run by the `fast` suite, not by `unit`.
"""
from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

EXCLUDE = {'.git', 'target', '.check-runs', '.venv', '__pycache__'}


def ignore(dirpath, names):
    dropped = {n for n in names if n in EXCLUDE}
    # spike/uml/work is derived UML build scratch (git-ignored, multi-GB), never copied.
    if Path(dirpath).relative_to(ROOT).as_posix() == 'spike/uml': dropped.add('work')
    return dropped


class CleanWorktreeBuildTests(unittest.TestCase):
    def test_clean_tree_builds_and_collects_tests(self):
        with tempfile.TemporaryDirectory() as td:
            tree = Path(td) / 'lightsandbox'
            shutil.copytree(ROOT, tree, ignore=ignore)
            r = subprocess.run([sys.executable, str(tree / 'dev' / 'check'), 'unit', '--json'],
                               capture_output=True, text=True, timeout=900)
            out = r.stdout + r.stderr
            self.assertEqual(r.returncode, 0, out[-4000:])
            self.assertIn('"tests_total"', r.stdout, r.stdout[-2000:])
            marker = '"tests_total": 0'
            self.assertNotIn(marker, r.stdout, 'zero tests collected must fail')
            summary = [line for line in r.stdout.splitlines() if '"tests_total"' in line][-1]
            self.assertRegex(summary, r'"tests_total": [1-9][0-9]*')


if __name__ == '__main__':
    unittest.main()
