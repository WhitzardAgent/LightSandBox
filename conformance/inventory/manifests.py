"""Canonical manifest bytes + hashing.

The manifest is JSONL, one entry per task, entries sorted by uid and keys
sorted inside each line. `manifest_bytes` is the single serialization entry
point so the on-disk file, the lock hash and every test compare the same
bytes.
"""
from __future__ import annotations

import hashlib
import json


def sort_entries(entries):
    return sorted(entries, key=lambda e: e['uid'])


def manifest_line(entry) -> str:
    return json.dumps(entry, sort_keys=True, ensure_ascii=False, separators=(',', ':'))


def manifest_bytes(entries) -> bytes:
    lines = [manifest_line(e) for e in sort_entries(entries)]
    return ('\n'.join(lines) + '\n').encode('utf-8')


def manifest_sha256(entries) -> str:
    return hashlib.sha256(manifest_bytes(entries)).hexdigest()


def read_manifest(path):
    entries = []
    for line in open(path, encoding='utf-8'):
        if line.strip():
            entries.append(json.loads(line))
    return entries
