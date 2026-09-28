"""T02-C02: a task without solution/PoC is kept, marked ORACLE_UNAVAILABLE,
and never counted as a positive oracle."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

import unittest
from pathlib import Path

from conformance.inventory import enumerate as inv
from conformance.inventory.oracle import ORACLE_AVAILABLE, ORACLE_UNAVAILABLE, ORACLE_NOT_YET_VERIFIED, summarize_oracle

FIXTURES = Path(__file__).resolve().parent / 'fixtures'
TB_CHECKOUT = FIXTURES / 'tb-checkout'


class OracleUnavailableTests(unittest.TestCase):
    def setUp(self):
        self.entries, self.diag = inv.enumerate_terminalbench(TB_CHECKOUT, split='tasks')
        self.by_uid = {e['uid']: e for e in self.entries}

    def test_task_without_solution_is_kept_and_flagged(self):
        entry = self.by_uid['terminalbench:tasks:c02-no-oracle']
        self.assertEqual(ORACLE_UNAVAILABLE, entry['oracle_status'])
        self.assertFalse(entry['oracle']['solution_present'])
        self.assertEqual([], entry['oracle']['solution_files'])

    def test_task_with_solution_is_not_yet_verified(self):
        # Present but not executed against the reference runtime: never claim
        # a verified positive oracle from asset presence alone.
        entry = self.by_uid['terminalbench:tasks:c01-gap-task']
        self.assertEqual(ORACLE_NOT_YET_VERIFIED, entry['oracle_status'])
        self.assertTrue(entry['oracle']['solution_present'])
        self.assertEqual(['solution/solve.sh'], entry['oracle']['solution_files'])

    def test_positive_oracle_count_excludes_unavailable_and_unverified(self):
        summary = summarize_oracle(self.entries)
        self.assertEqual(0, summary[ORACLE_AVAILABLE])
        self.assertEqual(1, summary[ORACLE_UNAVAILABLE])
        self.assertEqual(len(self.entries) - 1, summary[ORACLE_NOT_YET_VERIFIED])
        # Positive-pass denominators may only use ORACLE_AVAILABLE entries.
        self.assertEqual(0, summary['positive_oracle_eligible'])
        self.assertEqual(len(self.entries), summary['total'])
        eligible_uids = [e['uid'] for e in self.entries
                         if e['oracle_status'] == ORACLE_AVAILABLE]
        self.assertNotIn('terminalbench:tasks:c02-no-oracle', eligible_uids)


if __name__ == '__main__':
    unittest.main()
