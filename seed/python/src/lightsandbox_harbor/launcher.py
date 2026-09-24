"""One-command Harbor launcher; refuses missing backend/source certification.

A dry run is an argv plan, NOT evidence that an environment can execute.
The actual native SandboxBackend transport must be implemented separately.
"""
from __future__ import annotations
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

PROVIDER = 'lightsandbox_harbor.provider:LightSandboxEnvironment'
CONTRACT_FILES = (
    'environments/base.py', 'environments/capabilities.py', 'environments/factory.py',
    'environments/definition.py', 'models/task/config.py', 'cli/jobs.py',
)


def build_argv(*, task: Path | None = None, dataset: str | None = None, agent: str = 'oracle') -> list[str]:
    if (task is None) == (dataset is None):
        raise ValueError('specify exactly one task directory or pinned dataset identifier')
    source = str(task) if task is not None else dataset
    if not source or source.startswith('-') or '\0' in source or not agent or agent.startswith('-') or '\0' in agent:
        raise ValueError('invalid source/agent')
    return ['harbor','run','-p' if task is not None else '-d',source,'--env',PROVIDER,'-a',agent]


def validate_lock(package_root: Path, lock: dict) -> None:
    if lock.get('status') != 'LOCKED':
        raise ValueError('Harbor source contract is UNLOCKED')
    files=lock.get('files',{})
    if not isinstance(files,dict) or not set(CONTRACT_FILES).issubset(files):
        raise ValueError('Harbor lock lacks required contract files')
    for rel in CONTRACT_FILES:
        path=package_root/rel
        if not path.is_file() or path.is_symlink():
            raise ValueError(f'missing/symlink Harbor contract source: {rel}')
        actual=hashlib.sha256(path.read_bytes()).hexdigest()
        if actual!=files[rel]:
            raise ValueError(f'installed Harbor contract hash mismatch: {rel}')


def main(argv: list[str] | None = None) -> int:
    parser=argparse.ArgumentParser(description=__doc__)
    source=parser.add_mutually_exclusive_group(required=True)
    source.add_argument('--task',type=Path);source.add_argument('--dataset')
    parser.add_argument('--agent',default='oracle')
    parser.add_argument('--dry-run',action='store_true')
    args=parser.parse_args(argv)
    try:
        command=build_argv(task=args.task,dataset=args.dataset,agent=args.agent)
        if args.dry_run:
            print(json.dumps({'argv':command,'execution_ready':False,'mode':'plan_only'},ensure_ascii=False,indent=2))
            return 0
        if not os.environ.get('LIGHTSANDBOX_BACKEND_FACTORY'):
            raise ValueError('native SandboxBackend not configured; the archive is not a running sandbox product')
        lock_path=os.environ.get('LIGHTSANDBOX_HARBOR_LOCK')
        if not lock_path:
            raise ValueError('LIGHTSANDBOX_HARBOR_LOCK is required; see T33')
        executable=shutil.which('harbor')
        if executable is None:
            raise ValueError('pinned Harbor executable not installed')
        import harbor
        locations=list(harbor.__path__)
        if len(locations)!=1:
            raise ValueError('ambiguous Harbor package root')
        validate_lock(Path(locations[0]),json.loads(Path(lock_path).read_text()))
        command[0]=executable
        # Never shell=True. No unrestricted argument passthrough that could
        # override --env or point the run back at Docker.
        return subprocess.run(command,check=False).returncode
    except (ValueError,OSError,ImportError) as exc:
        print(f'BLOCKED: {exc}',file=sys.stderr)
        return 2

if __name__=='__main__':raise SystemExit(main())
