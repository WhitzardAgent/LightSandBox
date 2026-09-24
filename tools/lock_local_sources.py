#!/usr/bin/env python3
"""Hash clean local Git sources. Never fetch, install, execute source or edit Git.

Default output is stdout. --output uses exclusive creation. A partial input set
produces PARTIAL; it is never an SDK/image/dataset lock or a release approval.
"""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import urlsplit

ROOT=Path(__file__).resolve().parents[1]


def canonical_origin(value: str) -> str:
    value=value.strip()
    if value.startswith('git@') and ':' in value:
        host,path=value.split(':',1);value='https://'+host.split('@',1)[1]+'/'+path
    u=urlsplit(value)
    return ((u.hostname or '').lower()+u.path).removesuffix('.git').rstrip('/')


def git(repo: Path,*args: str) -> bytes:
    env=os.environ.copy()
    env.update(GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL=os.devnull,GIT_TERMINAL_PROMPT='0',GIT_OPTIONAL_LOCKS='0')
    result=subprocess.run(['git','-c','core.fsmonitor=false','-c','core.untrackedCache=false','-c',f'core.hooksPath={os.devnull}',
                           '-C',str(repo),*args],capture_output=True,env=env,timeout=30)
    if result.returncode:
        raise ValueError(f'git read failed ({args[0]}) in local source: {result.stderr.decode(errors="replace")[:600]}')
    return result.stdout


def lock_one(item: dict, repo: Path) -> dict:
    repo=repo.resolve()
    if not repo.is_dir():raise ValueError(f'{item["name"]}: local source directory missing')
    if git(repo,'status','--porcelain','--untracked-files=normal').strip():
        raise ValueError(f'{item["name"]}: dirty source; use a clean tracked checkout')
    origin=git(repo,'remote','get-url','origin').decode().strip()
    if canonical_origin(origin)!=canonical_origin(item['url']):
        raise ValueError(f'{item["name"]}: origin mismatch (expected registered repository)')
    rev=git(repo,'rev-parse','--verify','HEAD^{commit}').decode().strip()
    if not re.fullmatch(r'[0-9a-f]{40}(?:[0-9a-f]{24})?',rev):raise ValueError('invalid complete Git revision')
    hashes={}
    for name in item['required_files']:
        if Path(name).is_absolute() or '..' in Path(name).parts:raise ValueError('unsafe required source path')
        try:content=git(repo,'show',f'{rev}:{name}')
        except ValueError as exc:raise ValueError(f'{item["name"]}: missing required source {name}') from exc
        hashes[name]=hashlib.sha256(content).hexdigest()
    return {**item,'revision':rev,'status':'LOCKED','file_hashes':hashes,
            'provenance':'clean_local_git_bytes_origin_matched_no_network'}


def main() -> int:
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo',action='append',default=[],metavar='NAME=LOCAL_PATH')
    parser.add_argument('--catalog',type=Path,default=ROOT/'specs/upstream-lock.json')
    parser.add_argument('--output',type=Path);parser.add_argument('--require-all',action='store_true')
    args=parser.parse_args()
    try:
        source=json.loads(args.catalog.read_text(encoding='utf-8'))
        items={x['name']:x for x in source['repositories']};local={}
        for value in args.repo:
            name,sep,path=value.partition('=')
            if not sep or not path or name not in items or name in local:raise ValueError('invalid, duplicate or unknown --repo name')
            local[name]=Path(path)
        if not local:raise ValueError('provide at least one --repo NAME=LOCAL_PATH')
        locked=[]
        for name,item in items.items():
            if name in local:locked.append(lock_one(item,local[name]))
            else:locked.append({k:v for k,v in {**item,'status':'UNLOCKED','revision':None}.items() if k not in ('file_hashes','provenance')})
        complete=all(i['status']=='LOCKED' for i in locked)
        if args.require_all and not complete:raise ValueError('not all registered sources provided; refusing complete lock')
        report={'schema_version':'2.0','status':'LOCKED' if complete else 'PARTIAL',
                'observed_at':datetime.now(timezone.utc).isoformat(),'repositories':locked,
                'scope':'source bytes only; SDK packages, binaries, image digests and dataset manifests require separate locks',
                'release_blocking':not complete}
        text=json.dumps(report,ensure_ascii=False,indent=2)+'\n'
        if args.output:
            with args.output.open('x',encoding='utf-8') as f:f.write(text)
        else:print(text,end='')
    except (OSError,ValueError,KeyError,subprocess.SubprocessError) as exc:
        print(f'lock failed: {exc}',file=sys.stderr);return 1
    return 0
if __name__=='__main__':raise SystemExit(main())
