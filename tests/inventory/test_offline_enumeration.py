"""Offline enumeration behavior for SWE-bench rows and CyberGym task lists:
distinct splits (never merged), dedupe flags, oracle status derivation, and
asset/verification bookkeeping."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

import json
import unittest
from pathlib import Path

from conformance.inventory import enumerate as inv
from conformance.inventory.oracle import ORACLE_NOT_YET_VERIFIED, ORACLE_UNAVAILABLE

FIXTURES = Path(__file__).resolve().parent / 'fixtures'


def load_rows(split):
    rows = []
    for line in (FIXTURES / 'swe-rows' / f'{split}.jsonl').read_text().splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


class SwebenchEnumerationTests(unittest.TestCase):
    def setUp(self):
        self.aliases = inv.load_dataset_aliases(FIXTURES / 'swe-checkout')
        self.rows = {s: load_rows(s) for s in ('full', 'lite', 'verified')}
        self.entries, self.diag = inv.enumerate_swebench(self.rows, dataset_revisions={
            'full': 'rev-full', 'lite': 'rev-lite', 'verified': 'rev-verified'})

    def test_aliases_are_read_from_checkout(self):
        self.assertEqual('SWE-bench/SWE-bench_Lite', self.aliases['lite'])
        self.assertEqual('SWE-bench/SWE-bench_Verified', self.aliases['verified'])

    def test_splits_stay_distinct_never_merged(self):
        uids = {e['uid'] for e in self.entries}
        # Same instance_id appears in full, lite and verified fixtures; each
        # split must keep its own entry.
        self.assertIn('swebench:full:django__django-10000', uids)
        self.assertIn('swebench:lite:django__django-10000', uids)
        self.assertIn('swebench:verified:django__django-10000', uids)
        splits = {e['split'] for e in self.entries}
        self.assertEqual({'full', 'lite', 'verified'}, splits)

    def test_duplicate_source_rows_are_deduped_and_flagged(self):
        full = [e for e in self.entries if e['split'] == 'full']
        # fixture ships 3 rows but one duplicate instance_id
        self.assertEqual(3, self.diag['raw_rows']['full'])
        self.assertEqual(2, len(full))
        dups = [e for e in full if e['dedupe_flags']]
        self.assertEqual(1, len(dups))
        self.assertIn('duplicate_instance_id_in_source', dups[0]['dedupe_flags'])

    def test_split_overlap_is_reported_not_merged(self):
        overlap = self.diag['split_overlap']
        self.assertIn('django__django-10000', overlap['full:lite'])
        self.assertIn('django__django-10000', overlap['full:verified'])
        self.assertIn('django__django-10000', overlap['lite:verified'])

    def test_oracle_status_from_gold_patch(self):
        by_uid = {e['uid']: e for e in self.entries}
        self.assertEqual(ORACLE_NOT_YET_VERIFIED,
                         by_uid['swebench:full:django__django-10000']['oracle_status'])
        self.assertEqual(ORACLE_UNAVAILABLE,
                         by_uid['swebench:full:psf__requests-2000']['oracle_status'])

    def test_entry_carries_source_provenance(self):
        for e in self.entries:
            self.assertEqual(self.aliases[e['split']], e['source']['dataset'])
            self.assertTrue(e['source']['dataset_revision'].startswith('rev-'))
            self.assertEqual('network_clone_filtered_at_locked_rev',
                             e['source']['harness_provenance'])


class CybergymEnumerationTests(unittest.TestCase):
    def setUp(self):
        self.tasks = json.loads((FIXTURES / 'cybergym-fetch' / 'tasks.json').read_text())
        self.api = json.loads((FIXTURES / 'cybergym-fetch' / 'dataset-api.json').read_text())
        self.entries, self.diag = inv.enumerate_cybergym(
            self.tasks, self.api, dataset_revision='rev-cybergym')

    def test_all_tasks_enumerated_with_family(self):
        self.assertEqual(3, len(self.entries))
        fams = {e['family'] for e in self.entries}
        self.assertEqual({'arvo', 'oss-fuzz'}, fams)
        self.assertEqual(['cybergym:main:arvo:10400',
                          'cybergym:main:arvo:1065',
                          'cybergym:main:oss-fuzz:42535201'],
                         sorted(e['uid'] for e in self.entries))

    def test_runner_assets_and_verification_entry_recorded(self):
        by_uid = {e['uid']: e for e in self.entries}
        arvo = by_uid['cybergym:main:arvo:1065']
        self.assertIn('n132/arvo:1065-vul', arvo['capabilities']['runner_images'])
        self.assertIn('n132/arvo:1065-fix', arvo['capabilities']['runner_images'])
        self.assertEqual('submission_server', arvo['capabilities']['verifier_topology'])
        oss = by_uid['cybergym:main:oss-fuzz:42535201']
        self.assertIn('cybergym/oss-fuzz:42535201-vul', oss['capabilities']['runner_images'])

    def test_asset_presence_and_gap_flags(self):
        by_uid = {e['uid']: e for e in self.entries}
        arvo = by_uid['cybergym:main:arvo:1065']
        self.assertEqual([], arvo['asset_missing'])
        oss = by_uid['cybergym:main:oss-fuzz:42535201']
        self.assertIn('repo-fix.tar.gz', oss['asset_missing'])
        self.assertIn('patch.diff', oss['asset_missing'])

    def test_no_public_poc_means_not_yet_verified_everywhere(self):
        for e in self.entries:
            self.assertEqual(ORACLE_NOT_YET_VERIFIED, e['oracle_status'])
            self.assertFalse(e['oracle']['public_poc_available'])

    def test_difficulty_modes_recorded(self):
        by_uid = {e['uid']: e for e in self.entries}
        self.assertEqual(['level0', 'level3'], by_uid['cybergym:main:arvo:1065']['difficulty_levels'])
        self.assertEqual(['level0'], by_uid['cybergym:main:oss-fuzz:42535201']['difficulty_levels'])


if __name__ == '__main__':
    unittest.main()
