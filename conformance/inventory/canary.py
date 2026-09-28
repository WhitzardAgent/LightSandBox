"""Coverage-driven canary selection (T02 canary step).

The canary is a feedback-shortening DEV subset of the full manifest. It is
derived purely from per-task field coverage: greedily keep, in stable uid
order, every task that introduces at least one previously-unseen field. That
makes selection deterministic and ensures that extending the manifest with
tasks that add no new field never changes the canary; only tasks carrying
new fields may *extend* it. Selection never mutates the manifest: the full
set stays the release denominator, reported via reference_sha256.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import sys

from .manifests import manifest_sha256, read_manifest


def cover_fields(entry, record=None):
    """The observable field set a task contributes to coverage."""
    caps = entry.get('capabilities', {})
    fields = set(caps.get('demands', []))
    fields.add(str(entry.get('oracle_status', '')).lower())
    topology = caps.get('verifier_topology')
    if topology:
        fields.add(f'verifier_topology:{topology}')
    for key in ('gpus', 'privileged', 'network_modes'):
        if caps.get(key):
            fields.add(str(key))
    if record:
        for unknown in record.get('unknown_keys', []):
            fields.add(f"unknown_key:{unknown['key']}")
    return fields


def select_canary(entries, records=None):
    """Greedy coverage selection.

    Input order is normalized to uid order first, so the output depends only
    on the entry set, not on file order. Returns a plan: sorted canary uids,
    a rationale row per selected task, the full field universe, and the
    sha256 of the manifest the plan was computed against.
    """
    ordered = sorted(entries, key=lambda e: e['uid'])
    by_uid_records = {}
    for rec in records or []:
        if rec.get('uid'):
            by_uid_records[rec['uid']] = rec
    universe = {}
    fields_of = {}
    for entry in ordered:
        fields = cover_fields(entry, by_uid_records.get(entry['uid']))
        fields_of[entry['uid']] = fields
        for field in fields:
            universe.setdefault(field, []).append(entry['uid'])
    covered = set()
    canary = []
    rationale = []
    for entry in ordered:
        new = sorted(fields_of[entry['uid']] - covered)
        if new:
            canary.append(entry['uid'])
            rationale.append({'uid': entry['uid'], 'covers': new})
            covered.update(new)
    uncovered = sorted(set(universe) - covered)
    return {
        'schema_version': '1.0',
        'canary': sorted(canary),
        'rationale': sorted(rationale, key=lambda r: r['uid']),
        'field_universe': sorted(universe),
        'coverage': {f: sorted(u) for f, u in sorted(universe.items())},
        'uncovered_fields': uncovered,
        'reference_sha256': manifest_sha256(entries),
        'total_entries': len(entries),
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True)
    parser.add_argument('--out')
    args = parser.parse_args(argv)
    entries = read_manifest(args.manifest)
    plan = select_canary(entries)
    blob = json.dumps(plan, sort_keys=True, ensure_ascii=False, indent=2) + '\n'
    if args.out:
        Path(args.out).write_text(blob, encoding='utf-8')
    print(json.dumps({'canary': len(plan['canary']), 'total': plan['total_entries'],
                      'reference_sha256': plan['reference_sha256'],
                      'uncovered_fields': plan['uncovered_fields']},
                     ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == '__main__':
    sys.exit(main())
