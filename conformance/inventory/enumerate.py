"""T02 task enumeration: build manifest entries from real benchmark sources.

* terminalbench -- walk tasks/ of the locked checkout (harbor task layout).
* swebench      -- instance rows of the HF datasets the locked harness repo
                   names (full/lite/verified stay distinct, never merged).
* cybergym      -- tasks.json + file inventory of the HF dataset the locked
                   README documents, plus the locked repo's runner/verifier
                   wiring (scripts/server_data/download.py, cybergym.server).

Everything here is deterministic: the same inputs always yield the same
manifest bytes. `check_consistency` is the C03 gate comparing a manifest
against the full upstream task-id list.
"""
from __future__ import annotations

import argparse
import ast
import hashlib
import json
from pathlib import Path
import sys

from . import census
from .manifests import manifest_bytes, read_manifest
from .oracle import (ORACLE_NOT_YET_VERIFIED, ORACLE_UNAVAILABLE,
                     summarize_oracle)

PROVENANCE = 'network_clone_filtered_at_locked_rev'

DEFAULT_SWE_ALIASES = {
    'full': 'SWE-bench/SWE-bench',
    'lite': 'SWE-bench/SWE-bench_Lite',
    'verified': 'SWE-bench/SWE-bench_Verified',
}
CYBERGYM_EXPECTED_ASSETS = ('description.txt', 'error.txt', 'patch.diff',
                            'repo-fix.tar.gz', 'repo-vul.tar.gz')


# ------------------------------------------------------------------ helpers

def _base_entry(benchmark, split, task_id):
    return {
        'benchmark': benchmark,
        'split': split,
        'family': '',
        'task_id': task_id,
        'uid': f'{benchmark}:{split}:{task_id}',
        'dedupe_flags': [],
        'oracle_status': ORACLE_NOT_YET_VERIFIED,
        'oracle': {},
        'capabilities': {'demands': []},
        'asset_missing': [],
        'source': {},
    }


# ------------------------------------------------------------- terminalbench

def load_task_digests(checkout):
    """Task content digests pinned in tasks/dataset.toml of the checkout."""
    path = Path(checkout) / 'tasks' / 'dataset.toml'
    if not path.is_file():
        return {}, None
    import tomllib
    data = tomllib.loads(path.read_text(encoding='utf-8'))
    digests = {}
    for task in data.get('tasks', []):
        digests[str(task['name']).split('/')[-1]] = task.get('digest')
    dataset = (data.get('dataset') or {}).get('name')
    return digests, dataset


def enumerate_terminalbench(checkout, split='tasks', repo='harbor-framework/terminal-bench',
                            revision=''):
    checkout = Path(checkout)
    tasks_dir = checkout / 'tasks'
    entries = []
    errors = []
    skipped = []
    records = []
    digests, dataset_name = load_task_digests(checkout)
    if tasks_dir.is_dir():
        for child in sorted(tasks_dir.iterdir()):
            if not child.is_dir():
                continue
            if not (child / 'task.toml').is_file():
                skipped.append(child.name)
                continue
            record = census.scan_task(child, 'terminalbench', split)
            records.append(record)
            errors.extend({'task_id': record['task_id'], **e}
                          for e in record['parse_errors'])
            entry = _base_entry('terminalbench', split, record['task_id'])
            entry['capabilities'] = census.record_capabilities(record)
            solutions = record['fields'].get('solution_files', [])
            entry['oracle'] = {
                'solution_present': bool(solutions),
                'solution_files': solutions,
                'instruction_present': bool(record['fields'].get('has_instruction')),
            }
            entry['oracle_status'] = (ORACLE_NOT_YET_VERIFIED if solutions
                                      else ORACLE_UNAVAILABLE)
            missing = []
            environment = record['fields']
            has_build = 'dockerfile' in entry['capabilities']['demands']
            if not has_build:
                missing.append('environment_image_source')
            if not environment.get('has_verifier_tests'):
                missing.append('verifier_tests')
            if not environment.get('has_instruction'):
                missing.append('instruction.md')
            entry['asset_missing'] = missing
            entry['source'] = {
                'repo': repo,
                'revision': revision,
                'path': f'tasks/{record["task_id"]}',
                'task_format': record['fields'].get('task_schema_version'),
                'provenance': PROVENANCE,
            }
            if digests:
                entry['source']['dataset_manifest'] = dataset_name
                entry['source']['digest'] = digests.get(record['task_id'])
            entries.append(entry)
    seen = {}
    for e in entries:
        seen[e['task_id']] = seen.get(e['task_id'], 0) + 1
    for task_id, n in sorted(seen.items()):
        if n > 1:
            for e in entries:
                if e['task_id'] == task_id:
                    e['dedupe_flags'].append('duplicate_task_dir_in_source')
    diag = {'errors': errors, 'task_dirs': len(entries), 'skipped': skipped,
            'dataset_manifest': dataset_name,
            'dataset_manifest_tasks': len(digests)}
    return entries, diag


# ------------------------------------------------------------------ swebench

def load_dataset_aliases(checkout):
    """Read DATASET_ALIASES from the locked harness checkout (no import)."""
    path = Path(checkout) / 'swebench' / 'cli' / '_datasets.py'
    tree = ast.parse(path.read_text(encoding='utf-8'), filename=str(path))
    for node in tree.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id == 'DATASET_ALIASES':
                    return ast.literal_eval(node.value)
    raise ValueError(f'DATASET_ALIASES not found in {path}')


def enumerate_swebench(rows_by_split, dataset_revisions, aliases=None,
                       harness='SWE-bench/SWE-bench', harness_revision='',
                       harness_provenance=PROVENANCE):
    """One manifest entry per dataset row, per split. Splits are never
    merged; duplicate instance ids inside one split source are deduped and
    flagged, and cross-split overlap is only reported in diagnostics."""
    aliases = dict(DEFAULT_SWE_ALIASES if aliases is None else aliases)
    entries = []
    raw_rows = {}
    kept = {}
    duplicates = {}
    ids_by_split = {}
    for split in sorted(rows_by_split):
        rows = rows_by_split[split]
        raw_rows[split] = len(rows)
        seen = {}
        order = []
        for row in rows:
            iid = row.get('instance_id')
            if not iid:
                raise ValueError(f'swebench row without instance_id in split {split}')
            if iid in seen:
                if 'duplicate_instance_id_in_source' not in seen[iid]['dedupe_flags']:
                    seen[iid]['dedupe_flags'].append('duplicate_instance_id_in_source')
                duplicates.setdefault(split, []).append(iid)
                continue
            entry = _base_entry('swebench', split, iid)
            patch = row.get('patch') or ''
            entry['oracle'] = {
                'gold_patch_present': bool(patch.strip()),
                'repo': row.get('repo'),
                'base_commit': row.get('base_commit'),
                'version': str(row.get('version') or ''),
                'created_at': row.get('created_at') or '',
            }
            entry['oracle_status'] = (ORACLE_NOT_YET_VERIFIED if patch.strip()
                                      else ORACLE_UNAVAILABLE)
            entry['capabilities'] = {
                'demands': ['harness_eval', 'per_repo_image'],
                'verifier_topology': 'harness_script',
                'repo': row.get('repo'),
                'version': str(row.get('version') or ''),
            }
            entry['source'] = {
                'dataset': aliases.get(split, split),
                'dataset_revision': dataset_revisions.get(split, ''),
                'harness': harness,
                'harness_revision': harness_revision,
                'harness_provenance': harness_provenance,
            }
            seen[iid] = entry
            order.append(iid)
        kept[split] = len(order)
        entries.extend(seen[iid] for iid in order)
        ids_by_split[split] = {seen[iid]['task_id'] for iid in order}
    overlap = {}
    splits = sorted(ids_by_split)
    for i, a in enumerate(splits):
        for b in splits[i + 1:]:
            overlap[f'{a}:{b}'] = sorted(ids_by_split[a] & ids_by_split[b])
    diag = {
        'raw_rows': raw_rows,
        'entries': kept,
        'duplicates': duplicates,
        'split_overlap': overlap,
        'errors': [],
    }
    return entries, diag


# ------------------------------------------------------------------ cybergym

def enumerate_cybergym(tasks, dataset_api, dataset_revision,
                       harness='sunblaze-ucb/cybergym', harness_revision='',
                       subset_ids=None, harness_provenance=PROVENANCE):
    siblings = {s['rfilename'] for s in dataset_api.get('siblings', [])}
    subset = set(subset_ids or [])
    entries = []
    duplicates = []
    seen = set()
    for task in tasks:
        task_id = task['task_id']
        family, _, ident = task_id.partition(':')
        files = {f for f in siblings if f.startswith(f'data/{family}/{ident}/')}
        names = {Path(f).name for f in files}
        entry = _base_entry('cybergym', 'main', task_id)
        entry['family'] = family
        if task_id in seen:
            entry['dedupe_flags'].append('duplicate_task_id_in_source')
            duplicates.append(task_id)
        seen.add(task_id)
        # Present but unexecuted: the public dataset ships no PoC assets, and
        # per docs/acceptance/02 the presence of public success PoCs must
        # never be assumed.
        poc_assets = sorted(f for f in files if 'poc' in Path(f).name.lower())
        entry['oracle'] = {
            'public_poc_available': bool(poc_assets),
            'public_poc_assets': poc_assets,
            'success_judgement': 'reference_runner_exit_code_not_executed',
        }
        entry['oracle_status'] = ORACLE_NOT_YET_VERIFIED
        if family == 'arvo':
            runners = [f'n132/arvo:{ident}-vul', f'n132/arvo:{ident}-fix']
        else:
            runners = [f'cybergym/oss-fuzz:{ident}-vul', f'cybergym/oss-fuzz:{ident}-fix']
        entry['capabilities'] = {
            'demands': ['per_task_runner_images', 'poc_submission_server',
                        'long_compile_debug'],
            'verifier_topology': 'submission_server',
            'runner_images': runners,
            'base_runner_image': 'cybergym/oss-fuzz-base-runner:latest',
            'project_language': task.get('project_language') or '',
            'project_main_repo': task.get('project_main_repo') or '',
        }
        levels = sorted((task.get('task_difficulty') or {}).keys())
        entry['difficulty_levels'] = levels
        entry['asset_missing'] = sorted(set(CYBERGYM_EXPECTED_ASSETS) - names)
        entry['assets_present'] = sorted(names & set(CYBERGYM_EXPECTED_ASSETS))
        if subset:
            entry['official_subset'] = task_id in subset
        entry['source'] = {
            'dataset': 'sunblaze-ucb/cybergym',
            'dataset_revision': dataset_revision,
            'dataset_files': len(siblings),
            'harness': harness,
            'harness_revision': harness_revision,
            'harness_provenance': harness_provenance,
        }
        entries.append(entry)
    families = {}
    for e in entries:
        families[e['family']] = families.get(e['family'], 0) + 1
    diag = {
        'errors': [],
        'tasks': len(entries),
        'families': families,
        'duplicates': duplicates,
        'dataset_sha': dataset_api.get('sha'),
        'dataset_files': len(siblings),
    }
    return entries, diag


# ------------------------------------------------------------------ c03 gate

def check_consistency(entries, upstream_ids):
    """Gate a manifest against the full upstream task-id list.

    Duplicate uids inside the manifest, ids missing from the manifest, and
    ids present but not upstream are all failures; each error names the exact
    offending set."""
    errors = []
    counts = {}
    for e in entries:
        counts[e['uid']] = counts.get(e['uid'], 0) + 1
    duplicates = sorted(uid for uid, n in counts.items() if n > 1)
    if duplicates:
        errors.append(f'duplicate uids in manifest: {duplicates}')
    manifest_ids = set(counts)
    upstream_set = set(upstream_ids)
    if len(upstream_set) != len(upstream_ids):
        seen, dup = set(), set()
        for uid in upstream_ids:
            (dup if uid in seen else seen).add(uid)
        errors.append(f'duplicate ids in upstream list: {sorted(dup)}')
    missing = sorted(upstream_set - manifest_ids)
    if missing:
        errors.append(f'missing vs upstream: {missing}')
    unexpected = sorted(manifest_ids - upstream_set)
    if unexpected:
        errors.append(f'unexpected vs upstream: {unexpected}')
    return errors


# ---------------------------------------------------------------------- cli

def _load_jsonl(path):
    return [json.loads(ln) for ln in Path(path).read_text(encoding='utf-8').splitlines()
            if ln.strip()]


def _write_manifest(entries, out):
    data = manifest_bytes(entries)
    Path(out).write_bytes(data)
    return hashlib.sha256(data).hexdigest()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='cmd', required=True)

    tb = sub.add_parser('terminalbench')
    tb.add_argument('--checkout', required=True)
    tb.add_argument('--split', default='tasks')
    tb.add_argument('--revision', default='')
    tb.add_argument('--out')

    swe = sub.add_parser('swebench')
    swe.add_argument('--checkout', required=True,
                     help='locked harness checkout; supplies DATASET_ALIASES')
    swe.add_argument('--harness-revision', default='')
    swe.add_argument('--rows-dir', help='dir with <split>.jsonl + <split>.meta.json')
    swe.add_argument('--fetch-dir',
                     help='fetch rows via hf.py into this dir, then enumerate')
    swe.add_argument('--splits', default='full,lite,verified')
    swe.add_argument('--out')

    cg = sub.add_parser('cybergym')
    cg.add_argument('--checkout', required=True)
    cg.add_argument('--harness-revision', default='')
    cg.add_argument('--tasks-file')
    cg.add_argument('--dataset-api')
    cg.add_argument('--fetch-dir', help='fetch tasks.json + dataset info via hf.py first')
    cg.add_argument('--subset-file', help='one official-subset task id per line')
    cg.add_argument('--out')

    gate = sub.add_parser('gate')
    gate.add_argument('--manifest', required=True)
    gate.add_argument('--upstream-ids', required=True,
                      help='text file, one full upstream uid per line')

    args = parser.parse_args(argv)
    if args.cmd == 'terminalbench':
        entries, diag = enumerate_terminalbench(args.checkout, split=args.split,
                                                revision=args.revision)
    elif args.cmd == 'swebench':
        aliases = load_dataset_aliases(args.checkout)
        wanted = [s.strip() for s in args.splits.split(',') if s.strip()]
        if args.fetch_dir:
            from . import hf
            hf.fetch_swebench_rows(aliases, wanted, Path(args.fetch_dir))
        rows, revisions = {}, {}
        for split in wanted:
            rows[split] = _load_jsonl(Path(args.rows_dir or args.fetch_dir) / f'{split}.jsonl')
            meta = json.loads(
                (Path(args.rows_dir or args.fetch_dir) / f'{split}.meta.json').read_text())
            revisions[split] = meta['dataset_sha']
        entries, diag = enumerate_swebench(rows, revisions, aliases=aliases,
                                           harness_revision=args.harness_revision)
    elif args.cmd == 'cybergym':
        if args.fetch_dir:
            from . import hf
            tasks_file, api_file = hf.fetch_cybergym_sources(Path(args.fetch_dir))
        else:
            tasks_file, api_file = Path(args.tasks_file), Path(args.dataset_api)
        tasks = json.loads(Path(tasks_file).read_text(encoding='utf-8'))
        api = json.loads(Path(api_file).read_text(encoding='utf-8'))
        subset = None
        if args.subset_file:
            subset = [ln.strip() for ln in Path(args.subset_file).read_text().splitlines()
                      if ln.strip()]
        entries, diag = enumerate_cybergym(tasks, api, api.get('sha', ''),
                                           harness_revision=args.harness_revision,
                                           subset_ids=subset)
    elif args.cmd == 'gate':
        entries = read_manifest(args.manifest)
        upstream = [ln.strip() for ln in
                    Path(args.upstream_ids).read_text(encoding='utf-8').splitlines()
                    if ln.strip()]
        errors = check_consistency(entries, upstream)
        print(json.dumps({'ok': not errors, 'errors': errors}, indent=2))
        return 1 if errors else 0
    summary = {
        'entries': len(entries),
        'oracle': summarize_oracle(entries),
        'diagnostics': diag,
    }
    if args.out:
        summary['manifest_sha256'] = _write_manifest(entries, args.out)
        summary['out'] = args.out
    print(json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == '__main__':
    sys.exit(main())
