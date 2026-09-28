"""T02-C04: coverage-derived canary. Regenerating over a superset only ever
changes the dev subset; it must never touch the release denominator (the full
manifest bytes stay byte-identical)."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

import copy
import unittest
from pathlib import Path

from conformance.inventory import census, enumerate as inv
from conformance.inventory.canary import select_canary
from conformance.inventory.manifests import manifest_bytes, manifest_sha256

FIXTURES = Path(__file__).resolve().parent / 'fixtures'
TB_CHECKOUT = FIXTURES / 'tb-checkout'


def clone_entry(entry, uid_suffix, mutate_caps=None):
    new = copy.deepcopy(entry)
    new['uid'] = f"{entry['uid']}:{uid_suffix}"
    new['task_id'] = f"{entry['task_id']}-{uid_suffix}"
    if mutate_caps:
        mutate_caps(new['capabilities'])
    return new


class CanaryTests(unittest.TestCase):
    def setUp(self):
        self.entries, self.diag = inv.enumerate_terminalbench(TB_CHECKOUT, split='tasks')
        self.records = census.scan_checkout(TB_CHECKOUT, 'terminalbench', 'tasks')

    def test_canary_covers_every_observed_field(self):
        plan = select_canary(self.entries, self.records)
        self.assertTrue(plan['canary'], 'canary must not be empty')
        covered = set()
        for item in plan['rationale']:
            covered.update(item['covers'])
        for field in ('devices', 'ulimits', 'gpus', 'oracle_unavailable', 'compose', 'healthchecks'):
            self.assertIn(field, covered, f'canary rationale missing coverage of {field}')
        self.assertLess(len(plan['canary']), len(self.entries),
                        'canary should be a real subset of the fixture manifest')
        for item in plan['rationale']:
            self.assertIn(item['uid'], plan['canary'])
            self.assertTrue(item['covers'], f"{item['uid']} selected but covers nothing new")

    def test_canary_selection_does_not_mutate_manifest(self):
        before = manifest_bytes(self.entries)
        before_sha = manifest_sha256(self.entries)
        select_canary(self.entries, self.records)
        self.assertEqual(before, manifest_bytes(self.entries))
        self.assertEqual(before_sha, manifest_sha256(self.entries))

    def test_superset_without_new_fields_keeps_canary(self):
        plan_base = select_canary(self.entries, self.records)
        superset = self.entries + [
            clone_entry(self.by_uid('c04-plain-a'), 'plain-c'),
            clone_entry(self.by_uid('c04-plain-b'), 'plain-d'),
        ]
        plan_sup = select_canary(superset, self.records)
        self.assertEqual(plan_base['canary'], plan_sup['canary'])
        # The plan binds the exact manifest bytes it was computed against,
        # and the original full set's hash is untouched by superset work.
        self.assertEqual(plan_base['reference_sha256'], manifest_sha256(self.entries))
        self.assertEqual(plan_sup['reference_sha256'], manifest_sha256(superset))

    def test_superset_with_new_field_extends_only_dev_subset(self):
        plan_base = select_canary(self.entries, self.records)
        before_bytes = manifest_bytes(self.entries)

        def add_privileged(caps):
            caps['demands'] = sorted(caps['demands'] + ['privileged'])
            caps['privileged'] = True

        exotic = clone_entry(self.by_uid('c04-plain-a'), 'exotic', add_privileged)
        superset = self.entries + [exotic]
        plan_sup = select_canary(superset, self.records)
        self.assertIn(exotic['uid'], plan_sup['canary'])
        self.assertTrue(set(plan_base['canary']).issubset(set(plan_sup['canary'])),
                        'a new field may only extend the dev subset, not churn it')
        # The full-set manifest is still what it was: canary work never
        # rewrites denominators.
        self.assertEqual(before_bytes, manifest_bytes(self.entries))
        self.assertEqual(plan_sup['reference_sha256'], manifest_sha256(superset))
        self.assertEqual(plan_base['reference_sha256'], manifest_sha256(self.entries))

    def by_uid(self, suffix):
        return next(e for e in self.entries if e['uid'].endswith(suffix))


if __name__ == '__main__':
    unittest.main()
