"""Tiny Hugging Face / datasets-server client used only by *real* T02
enumeration runs (stdlib urllib; offline tests never import this).

Every response is persisted to the given fetch directory before reduction,
so the counts in specs/ are always traceable to a saved artifact on the
fetching host. Any network or payload failure raises FetchError carrying the
raw message; callers must turn that into a BLOCKED record, never into
half-filled counts.
"""
from __future__ import annotations

import json
from pathlib import Path
import time
import urllib.error
import urllib.parse
import urllib.request

DATASETS_API = 'https://huggingface.co/api/datasets'
DATASETS_SERVER = 'https://datasets-server.huggingface.co'


class FetchError(RuntimeError):
    pass


def get_json(url: str, timeout: int = 120):
    request = urllib.request.Request(url, headers={'User-Agent': 'lightsandbox-t02/1.0'})
    attempt = 0
    while True:
        attempt += 1
        try:
            with urllib.request.urlopen(request, timeout=timeout) as resp:
                body = resp.read()
            break
        except urllib.error.HTTPError as exc:
            if exc.code in (429, 502, 503, 504) and attempt <= 8:
                retry_after = exc.headers.get('Retry-After') if exc.headers else None
                try:
                    wait = min(90.0, float(retry_after)) if retry_after else min(90.0, 2 ** attempt * 2)
                except ValueError:
                    wait = min(90.0, 2 ** attempt * 2)
                time.sleep(wait)
                continue
            raise FetchError(f'{url}: HTTP {exc.code} {exc.reason}') from exc
        except urllib.error.URLError as exc:
            raise FetchError(f'{url}: {exc}') from exc
    try:
        return json.loads(body.decode('utf-8'))
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise FetchError(f'{url}: unparsable JSON ({exc})') from exc


def dataset_info(slug: str) -> dict:
    return get_json(f'{DATASETS_API}/{urllib.parse.quote(slug)}')


def dataset_size(slug: str) -> dict:
    return get_json(f'{DATASETS_SERVER}/size?dataset={urllib.parse.quote(slug)}')


def dataset_rows(slug: str, config: str, split: str, page: int = 100) -> list[dict]:
    rows = []
    offset = 0
    while True:
        url = (f'{DATASETS_SERVER}/rows?dataset={urllib.parse.quote(slug)}'
               f'&config={urllib.parse.quote(config)}&split={urllib.parse.quote(split)}'
               f'&offset={offset}&length={page}')
        payload = get_json(url)
        batch = payload.get('rows') or []
        for item in batch:
            rows.append(item.get('row') or {})
        if len(batch) < page:
            return rows
        offset += page


def fetch_swebench_rows(aliases: dict, splits: list[str], out_dir: Path) -> None:
    """Fetch and persist instance rows for the given SWE-bench splits.

    Writes <split>.jsonl (full rows, one per line) and <split>.meta.json
    (dataset slug, git sha, datasets-server-reported size, measured rows)."""
    out_dir.mkdir(parents=True, exist_ok=True)
    for split in sorted(splits):
        slug = aliases[split]
        info = dataset_info(slug)
        size = dataset_size(slug)
        (out_dir / f'{split}.info.json').write_text(
            json.dumps({'dataset': slug, 'info': info, 'size': size},
                       ensure_ascii=False, indent=2, sort_keys=True) + '\n',
            encoding='utf-8')
        size_info = size.get('size') or {}
        splits_info = size_info.get('splits') or []
        configs = size_info.get('configs') or [{'config': info.get('config') or 'default'}]
        fetched = []
        errors = []
        for cfg in splits_info:
            meta = {'dataset': slug, 'dataset_sha': info.get('sha'),
                    'config': cfg['config'], 'split': cfg['split'],
                    'reported_num_examples': cfg.get('num_examples')}
            try:
                rows = dataset_rows(slug, cfg['config'], cfg['split'])
            except FetchError as exc:
                errors.append(str(exc))
                continue
            meta['measured_rows'] = len(rows)
            fetched.append(meta)
            suffix = f"{split}.{cfg['split']}" if cfg['split'] != 'test' else split
            with (out_dir / f'{suffix}.jsonl').open('w', encoding='utf-8') as fh:
                for row in rows:
                    fh.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + '\n')
        meta_all = {
            'dataset': slug,
            'dataset_sha': info.get('sha'),
            'last_modified': info.get('lastModified'),
            'configs_seen': [c.get('config') for c in configs],
            'fetched': fetched,
            'errors': errors,
        }
        (out_dir / f'{split}.meta.json').write_text(
            json.dumps(meta_all, ensure_ascii=False, indent=2, sort_keys=True) + '\n',
            encoding='utf-8')
        if errors:
            raise FetchError(f'{slug}: {errors}')
        # The row file the enumerator consumes is always <split>.jsonl; if the
        # datasets-server exposes a non-'test' split name, align the alias.
        named = out_dir / f'{split}.jsonl'
        if not named.exists():
            produced = sorted(out_dir.glob(f'{split}.*.jsonl'))
            if len(produced) == 1:
                produced[0].rename(named)
            else:
                raise FetchError(
                    f'{slug}: cannot identify the canonical split file ({produced})')


def fetch_cybergym_sources(out_dir: Path):
    """Fetch the CyberGym dataset api payload + tasks.json at the recorded
    dataset revision; returns (tasks_file, api_file)."""
    out_dir.mkdir(parents=True, exist_ok=True)
    info = dataset_info('sunblaze-ucb/cybergym')
    api_file = out_dir / 'cybergym-dataset-api.json'
    api_file.write_text(json.dumps(info, ensure_ascii=False, indent=2,
                                   sort_keys=True) + '\n', encoding='utf-8')
    sha = info.get('sha')
    url = (f'https://huggingface.co/datasets/sunblaze-ucb/cybergym'
           f'/resolve/{urllib.parse.quote(sha)}/tasks.json')
    tasks = get_json(url, timeout=300)
    tasks_file = out_dir / 'cybergym-tasks.json'
    tasks_file.write_text(json.dumps(tasks, ensure_ascii=False, indent=1,
                                     sort_keys=True) + '\n', encoding='utf-8')
    return tasks_file, api_file
