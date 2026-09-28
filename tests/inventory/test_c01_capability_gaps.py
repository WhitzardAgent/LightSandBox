"""T02-C01: a task with devices/ulimits/unknown compose fields stays in the
manifest, and every demanded capability plus the unknown key is listed."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

import unittest
from pathlib import Path

from conformance.inventory import census, enumerate as inv

FIXTURES = Path(__file__).resolve().parent / 'fixtures'
TB_CHECKOUT = FIXTURES / 'tb-checkout'


class CapabilityGapTests(unittest.TestCase):
    def setUp(self):
        self.entries, self.diag = inv.enumerate_terminalbench(TB_CHECKOUT, split='tasks')
        self.by_uid = {e['uid']: e for e in self.entries}
        self.records = census.scan_checkout(TB_CHECKOUT, 'terminalbench', 'tasks')
        self.census = census.build_census(self.records)

    def test_task_stays_in_manifest(self):
        uid = 'terminalbench:tasks:c01-gap-task'
        self.assertIn(uid, self.by_uid, f'manifest uids: {sorted(self.by_uid)}')
        self.assertEqual([], self.diag['errors'])

    def test_demanded_capabilities_are_listed(self):
        entry = self.by_uid['terminalbench:tasks:c01-gap-task']
        dem = entry['capabilities']['demands']
        for want in ('devices', 'ulimits', 'cap_add', 'shm_size', 'networks', 'volumes'):
            self.assertIn(want, dem, f'missing demand {want} in {dem}')
        self.assertIn('/dev/fuse', ' '.join(map(str, entry['capabilities']['devices'])))
        self.assertIn('nofile', entry['capabilities']['ulimits'])
        self.assertIn('SYS_PTRACE', entry['capabilities']['cap_add'])
        self.assertEqual('separate', entry['capabilities']['verifier_topology'])

    def test_compose_service_shape_recorded(self):
        entry = self.by_uid['terminalbench:tasks:c01-gap-task']
        self.assertEqual(['main', 'sidecar-db'], sorted(entry['capabilities']['compose_services']))
        self.assertTrue(entry['capabilities']['healthchecks'])

    def test_unknown_compose_key_preserved(self):
        rec = next(r for r in self.records if r['task_id'] == 'c01-gap-task')
        unknown = rec['unknown_keys']
        paths = {(u['file'], u['key']) for u in unknown}
        self.assertIn(('environment/docker-compose.yaml', 'quantum_entanglement_protocol'), paths)
        value = next(u['value'] for u in unknown if u['key'] == 'quantum_entanglement_protocol')
        self.assertEqual('enabled', value)

    def test_census_histogram_counts_fixtures(self):
        h = self.census['field_histogram']
        self.assertEqual(1, h['devices']['tasks'])
        self.assertEqual(1, h['ulimits']['tasks'])
        self.assertGreaterEqual(h['verifier_topology']['shared'], 1)
        self.assertGreaterEqual(h['verifier_topology']['separate'], 1)
        self.assertTrue(self.census['unknown_keys'], 'unknown keys must aggregate into census')
        self.assertIn('quantum_entanglement_protocol',
                      {u['key'] for u in self.census['unknown_keys']})


if __name__ == '__main__':
    unittest.main()
