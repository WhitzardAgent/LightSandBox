"""Reproducibility: regenerating the manifest + census from the checked-in
fixtures must reproduce the checked-in goldens byte-for-byte. This guards the
idempotence law for the real enumeration outputs (same inputs, same bytes)."""
from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

import json
import unittest
from pathlib import Path

from conformance.inventory import census, enumerate as inv
from conformance.inventory.manifests import manifest_bytes, manifest_sha256

FIXTURES = Path(__file__).resolve().parent / 'fixtures'
GOLDENS = FIXTURES / 'golden'


def build_fixture_manifest():
    entries = []
    tb, _ = inv.enumerate_terminalbench(FIXTURES / 'tb-checkout', split='tasks')
    entries.extend(tb)
    rows = {}
    for split in ('full', 'lite', 'verified'):
        rows[split] = [json.loads(ln) for ln in
                       (FIXTURES / 'swe-rows' / f'{split}.jsonl').read_text().splitlines()
                       if ln.strip()]
    swe, _ = inv.enumerate_swebench(rows, dataset_revisions={
        'full': 'rev-full', 'lite': 'rev-lite', 'verified': 'rev-verified'})
    entries.extend(swe)
    tasks = json.loads((FIXTURES / 'cybergym-fetch' / 'tasks.json').read_text())
    api = json.loads((FIXTURES / 'cybergym-fetch' / 'dataset-api.json').read_text())
    cg, _ = inv.enumerate_cybergym(tasks, api, dataset_revision='rev-cybergym')
    entries.extend(cg)
    return entries


class GoldenTests(unittest.TestCase):
    def test_manifest_matches_golden_bytes(self):
        entries = build_fixture_manifest()
        golden = (GOLDENS / 'fixture-manifest.jsonl').read_bytes()
        self.assertEqual(golden, manifest_bytes(entries),
                         'regenerated manifest differs from checked-in golden')

    def test_manifest_sha256_stable_across_runs(self):
        first = manifest_sha256(build_fixture_manifest())
        second = manifest_sha256(build_fixture_manifest())
        self.assertEqual(first, second)
        recorded = json.loads((GOLDENS / 'fixture-manifest.sha256.json').read_text())
        self.assertEqual(recorded['sha256'], first)
        self.assertEqual(recorded['entries'],
                         sum(1 for _ in
                             (GOLDENS / 'fixture-manifest.jsonl').read_text().splitlines() if _))

    def test_census_matches_golden_bytes(self):
        records = census.scan_checkout(FIXTURES / 'tb-checkout', 'terminalbench', 'tasks')
        blob = census.census_bytes(census.build_census(records))
        self.assertEqual((GOLDENS / 'fixture-census.json').read_bytes(), blob)


if __name__ == '__main__':
    unittest.main()
