#!/usr/bin/env python3
"""Offline planning-pack checks. No container, kernel or network operations.

This tool checks structure and evidence references, not the trustworthiness of a
CI issuer and not runtime correctness. Product release requires independent CI.
"""
from __future__ import annotations
import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
from urllib.parse import unquote, urlsplit

DEFAULT_ROOT = Path(__file__).resolve().parents[1]
STATES = {'planned','in_progress','implemented','in_review','verified','blocked','failed'}
HEX = re.compile(r'^[0-9a-f]{40}(?:[0-9a-f]{24})?$')

def digest(path: Path) -> str:
    h=hashlib.sha256()
    with path.open('rb') as f:
        for chunk in iter(lambda:f.read(1024*1024),b''): h.update(chunk)
    return h.hexdigest()

def safe_path(root: Path, name: str) -> Path:
    if not isinstance(name,str) or not name or Path(name).is_absolute():
        raise ValueError(f'unsafe relative path: {name!r}')
    target=(root/name).resolve()
    if not target.is_relative_to(root.resolve()):
        raise ValueError(f'unsafe path escapes pack: {name}')
    return target

def load(root: Path, name: str):
    path=safe_path(root,name)
    if not path.is_file(): raise ValueError(f'missing file: {name}')
    try:return json.loads(path.read_text(encoding='utf-8'))
    except (json.JSONDecodeError,UnicodeError) as exc:raise ValueError(f'JSON error {name}: {exc}') from exc

def file_set(root: Path):
    # spike/uml/work is derived UML build scratch: git-ignored, multi-GB, not manifest content.
    return {p.relative_to(root).as_posix():p for p in root.rglob('*')
            if p.is_file() and '__pycache__' not in p.parts and '.git' not in p.parts and p.name!='MANIFEST.json'
            and p.relative_to(root).parts[:3]!=('spike','uml','work')}

def indexed(values, label: str, errors: list[str]):
    result={}
    for value in values:
        key=value.get('id')
        if not isinstance(key,str):errors.append(f'{label} missing id');continue
        if key in result: errors.append(f'duplicate {label} id: {key}')
        result[key]=value
    return result

def validate_evidence(root: Path, task: dict, state: dict, errors: list[str]):
    tid=task['id'];refs=state.get('evidence',[])
    if not refs:errors.append(f'{tid}: verified requires evidence')
    review=state.get('review')
    if not isinstance(review,dict) or review.get('decision')!='approve' or not review.get('reviewer_id'):
        errors.append(f'{tid}: verified requires independent review record')
    covered=set()
    for ref in refs:
        try:
            report=load(root,ref)
            required=load(root,'specs/evidence-contract.json')['task_evidence_required']
            missing=set(required)-report.keys()
            if missing:errors.append(f'{tid}: evidence missing fields {sorted(missing)}');continue
            if report['task_id']!=tid:errors.append(f'{tid}: evidence task mismatch')
            if not HEX.fullmatch(str(report['candidate_commit'])):errors.append(f'{tid}: invalid evidence commit')
            if report['source_lock_sha256']!=digest(root/'specs/upstream-lock.json'):
                errors.append(f'{tid}: evidence source lock mismatch')
            collected=report['collected'];passed=report['passed'];failed=report['failed'];blocked=report['blocked']
            if not all(type(v) is int and v>=0 for v in (collected,passed,failed,blocked)) or collected<1 or passed!=collected or failed or blocked:
                errors.append(f'{tid}: evidence has zero, failed or blocked tests')
            if not report['commands'] or any(c.get('exit_code')!=0 or not c.get('argv') for c in report['commands']):
                errors.append(f'{tid}: evidence commands not successful')
            if not report['raw_artifacts']:errors.append(f'{tid}: missing raw evidence artifacts')
            for artifact in report['raw_artifacts']:
                p=safe_path(root,artifact['path'])
                if not p.is_file() or digest(p)!=artifact.get('sha256'):errors.append(f'{tid}: artifact hash mismatch {artifact["path"]}')
            covered.update(report['test_ids'])
            if isinstance(review,dict) and review.get('candidate_commit')!=report['candidate_commit']:
                errors.append(f'{tid}: review/evidence commit mismatch')
        except (ValueError,KeyError,TypeError,OSError) as exc: errors.append(f'{tid}: evidence {exc}')
    missing=set(task['test_ids'])-covered
    if missing:errors.append(f'{tid}: evidence missing required cases {sorted(missing)}')

def validate(root: Path) -> list[str]:
    errors=[]
    try:
        ts=load(root,'specs/task-catalog.json')['tasks']
        cs=load(root,'specs/test-catalog.json')['tests']
        rs=load(root,'specs/requirements.json')['requirements']
        ss=load(root,'state/project-status.json')['tasks']
        tasks=indexed(ts,'task',errors); cases=indexed(cs,'test',errors)
        reqs=indexed(rs,'requirement',errors); states=indexed(ss,'state',errors)
        if set(tasks)!=set(states):errors.append('state/task inventory mismatch')
        visiting=set();done=set()
        def visit(tid):
            if tid in visiting:errors.append(f'dependency cycle at {tid}');return
            if tid in done:return
            visiting.add(tid)
            for dep in tasks[tid]['depends_on']:
                if dep not in tasks:errors.append(f'{tid}: unknown dependency {dep}')
                else:visit(dep)
            visiting.remove(tid);done.add(tid)
        for tid in tasks:visit(tid)
        for tid,t in tasks.items():
            for name in [t['doc'],*t['read']]:
                if not safe_path(root,name).is_file(): errors.append(f'{tid}: missing document {name}')
            if not t['test_ids']:errors.append(f'{tid}: no test cases')
            for cid in t['test_ids']:
                if cid not in cases or cases[cid].get('task_id')!=tid:errors.append(f'{tid}: unknown or wrongly owned test {cid}')
            for rid in t['requirements']:
                if rid not in reqs:errors.append(f'{tid}: unknown requirement {rid}')
            s=states.get(tid,{})
            if s.get('status') not in STATES:errors.append(f'{tid}: invalid status {s.get("status")}')
            if s.get('status')=='verified':
                for dep in t['depends_on']:
                    if states.get(dep,{}).get('status')!='verified':errors.append(f'{tid}: dependency {dep} not verified')
                validate_evidence(root,t,s,errors)
        for cid,c in cases.items():
            if c.get('task_id') not in tasks or cid not in tasks[c['task_id']]['test_ids']:
                errors.append(f'orphan or unmapped test {cid}')
            if not all(c.get(k) for k in ('given','when','then')):errors.append(f'{cid}: incomplete test specification')
        for rid,r in reqs.items():
            want_t={t['id'] for t in ts if rid in t['requirements']}
            want_c={c['id'] for c in cs if rid in c['requirements']}
            if not want_t or set(r['tasks'])!=want_t or set(r['tests'])!=want_c:
                errors.append(f'{rid}: requirement mapping mismatch')
        features=load(root,'specs/feature-matrix.json')['features']
        indexed(features,'feature',errors)
        for f in features:
            if f.get('owner_task') not in tasks:
                errors.append(f'feature {f.get("id")}: unknown owner')
            if f.get('test_id') not in cases or cases[f['test_id']].get('task_id')!=f.get('owner_task'):
                errors.append(f'feature {f.get("id")}: invalid test mapping')
        # Parse all JSON, including schemas and source files, not only catalogs.
        # spike/uml/work is excluded for the same reason as in file_set():
        # derived git-ignored build scratch (kernel/go tool trees carry
        # intentionally malformed testdata JSON).
        for p in root.rglob('*.json'):
            if ('.git' not in p.parts and p.name!='MANIFEST.json'
                    and p.relative_to(root).parts[:3]!=('spike','uml','work')):
                load(root,p.relative_to(root).as_posix())
        # Internal Markdown file links, excluding external URLs and anchors.
        link=re.compile(r'(?<!!)\[[^\]]+\]\(([^\s)]+)(?:\s+"[^"]*")?\)')
        for p in root.rglob('*.md'):
            if '.git' in p.parts:continue
            if p.relative_to(root).parts[:3]==('spike','uml','work'):continue
            for target in link.findall(p.read_text(encoding='utf-8')):
                u=urlsplit(target)
                if u.scheme or u.netloc or not u.path:continue
                relative=(p.parent/unquote(u.path)).resolve()
                if not relative.is_relative_to(root.resolve()) or not relative.exists():
                    errors.append(f'broken/unsafe link: {p.relative_to(root)} -> {target}')
    except (ValueError,KeyError,TypeError,OSError) as exc:errors.append(str(exc))
    return errors

def release_blockers(root: Path) -> list[str]:
    problems=validate(root)
    if problems:return problems
    locks=['specs/upstream-lock.json','specs/sdk-lock.json','specs/runtime-artifacts-lock.json','specs/benchmark-lock.json']
    for name in locks:
        try:
            lock=load(root,name)
            if lock.get('status')!='LOCKED':problems.append(f'{name}: UNLOCKED or incomplete')
            if name.endswith('upstream-lock.json'):
                for r in lock['repositories']:
                    if not HEX.fullmatch(str(r.get('revision',''))) or not r.get('file_hashes'):
                        problems.append(f'{name}: incomplete immutable lock for {r["name"]}')
        except ValueError as exc:problems.append(str(exc))
    state=load(root,'state/project-status.json')
    unverified=[s['id'] for s in state['tasks'] if s['status']!='verified']
    if unverified:problems.append('unverified product tasks: '+', '.join(unverified))
    if state.get('host_status')!='VERIFIED':problems.append('host NOT_PROBED or unverified')
    if state.get('benchmark_status')!='FULL_VERIFIED':problems.append('complete benchmark ledger NOT_ENUMERATED or unverified')
    report=root/'release/release-gate.json'
    if not report.is_file():problems.append('missing independent release/release-gate.json')
    else:
        r=load(root,'release/release-gate.json')
        if r.get('decision')!='approve' or not r.get('trusted_ci_job') or not r.get('reviewer_id'):
            problems.append('independent release approval incomplete')
    return problems

def verify_manifest(root: Path) -> list[str]:
    errors=[]
    try:
        entries=load(root,'MANIFEST.json')['files'];actual=file_set(root)
        if set(entries)!=set(actual):
            errors.append(f'manifest file set mismatch: missing={sorted(set(entries)-set(actual))}, unexpected={sorted(set(actual)-set(entries))}')
        for name,record in entries.items():
            p=safe_path(root,name)
            if not p.is_file() or p.stat().st_size!=record['bytes'] or digest(p)!=record['sha256']:
                errors.append(f'manifest mismatch: {name}')
    except (ValueError,KeyError,TypeError,OSError) as exc:errors.append(str(exc))
    return errors

def main() -> int:
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root',type=Path,default=DEFAULT_ROOT)
    sub=parser.add_subparsers(dest='command',required=True)
    for cmd in ('validate','next','status','release-check','verify-manifest'):sub.add_parser(cmd)
    context=sub.add_parser('context');context.add_argument('task_id')
    args=parser.parse_args();root=args.root.resolve()
    if not root.is_dir():parser.error('root directory does not exist')
    if args.command in ('validate','release-check','verify-manifest'):
        errors={'validate':validate,'release-check':release_blockers,'verify-manifest':verify_manifest}[args.command](root)
        print(json.dumps({'command':args.command,'ok':not errors,'errors':errors,
                          'scope':'planning structure only; not runtime certification'},ensure_ascii=False,indent=2))
        return 1 if errors else 0
    errors=validate(root)
    if errors:
        print(json.dumps({'ok':False,'errors':errors},ensure_ascii=False,indent=2));return 1
    tasks=load(root,'specs/task-catalog.json')['tasks'];states={s['id']:s for s in load(root,'state/project-status.json')['tasks']}
    if args.command=='next':
        ready=[{'id':t['id'],'title':t['title'],'doc':t['doc']} for t in tasks if states[t['id']]['status']=='planned' and all(states[d]['status']=='verified' for d in t['depends_on'])]
        print(json.dumps({'ready':ready,'note':'This command never executes or changes task state.'},ensure_ascii=False,indent=2))
    elif args.command=='status':
        counts={s:sum(v['status']==s for v in states.values()) for s in sorted(STATES)}
        print(json.dumps({'counts':counts,'product_status':load(root,'state/project-status.json')['product_status']},ensure_ascii=False,indent=2))
    else:
        t=next((t for t in tasks if t['id']==args.task_id),None)
        if not t:print(f'unknown task: {args.task_id}',file=sys.stderr);return 2
        cases=[c for c in load(root,'specs/test-catalog.json')['tests'] if c['task_id']==t['id']]
        print(json.dumps({'task':t,'read_files':['AGENTS.md','START_HERE.md',t['doc'],*t['read']],
                          'dependency_states':{d:states[d]['status'] for d in t['depends_on']},'test_cases':cases},ensure_ascii=False,indent=2))
    return 0

if __name__=='__main__':raise SystemExit(main())
