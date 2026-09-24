"""dev/check CLI parse regression (round-01 review finding #9):
a `--flag` followed by another `--flag` must NOT swallow it as a value."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'dev'))

import checklib  # noqa: E402

STUB = {'exit': 3, 'suite': 'fast', 'commands': [], 'tests_total': 0,
        'failures': 0, 'blocked': 0, 'reason': 'stub'}


def captured_opts(argv):
    seen = {}

    def fake(suite, root, **opts):
        seen['suite'] = suite
        seen['opts'] = opts
        return dict(STUB, suite=suite)

    with mock.patch.object(checklib, 'run_suite', fake):
        checklib.main(argv)
    return seen


class CliParseTests(unittest.TestCase):
    def test_value_still_binds(self):
        seen = captured_opts(['fast', '--task', 'T41'])
        self.assertEqual(seen['opts'].get('task'), 'T41')

    def test_flag_never_swallowed_as_value(self):
        seen = captured_opts(['fast', '--task', '--json'])
        self.assertIs(seen['opts'].get('task'), True,
                      "--task followed by another flag must degrade to a boolean flag")
        self.assertIs(seen['opts'].get('json'), True)


if __name__ == '__main__':
    unittest.main()
