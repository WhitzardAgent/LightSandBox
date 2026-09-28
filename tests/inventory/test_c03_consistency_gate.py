"""T02-C03: duplicate uids, or divergence from the full upstream task-id
list, fail the consistency gate with the exact diff set in the report."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

import copy
import unittest
from pathlib import Path

from conformance.inventory import enumerate as inv

FIXTURES = Path(__file__).resolve().parent / 'fixtures'
TB_CHECKOUT = FIXTURES / 'tb-checkout'
UPSTREAM_IDS_FILE = FIXTURES / 'c03-upstream-ids.txt'


class ConsistencyGateTests(unittest.TestCase):
    def setUp(self):
        self.entries, self.diag = inv.enumerate_terminalbench(TB_CHECKOUT, split='tasks')
        self.upstream_ids = [ln.strip() for ln in UPSTREAM_IDS_FILE.read_text().splitlines() if ln.strip()]

    def test_fixture_manifest_matches_upstream_list(self):
        errors = inv.check_consistency(self.entries, self.upstream_ids)
        self.assertEqual([], errors)

    def test_duplicate_uid_fails_with_offender_named(self):
        duped = self.entries + [copy.deepcopy(self.entries[0])]
        errors = inv.check_consistency(duped, self.upstream_ids)
        self.assertTrue(errors, 'gate must fail on a duplicate')
        joined = ' | '.join(errors)
        self.assertIn('duplicate', joined)
        self.assertIn(self.entries[0]['uid'], joined)

    def test_missing_id_fails_with_exact_diff(self):
        trimmed = [e for e in self.entries if not e['uid'].endswith('c04-plain-b')]
        errors = inv.check_consistency(trimmed, self.upstream_ids)
        self.assertTrue(errors, 'gate must fail on a missing task')
        joined = ' | '.join(errors)
        self.assertIn('terminalbench:tasks:c04-plain-b', joined)
        self.assertIn('missing', joined)

    def test_extra_id_fails_with_exact_diff(self):
        extra = copy.deepcopy(self.entries[-1])
        extra['uid'] = 'terminalbench:tasks:not-in-upstream'
        extra['task_id'] = 'not-in-upstream'
        errors = inv.check_consistency(self.entries + [extra], self.upstream_ids)
        self.assertTrue(errors, 'gate must fail on an unexpected task')
        joined = ' | '.join(errors)
        self.assertIn('not-in-upstream', joined)
        self.assertIn('unexpected', joined)


if __name__ == '__main__':
    unittest.main()
