#!/usr/bin/env python3
"""coding-workspace: stage a source tree as an OCI-style workload plan.

The workspace fixture (workspace/calc.py + workspace/test_calc.py) contains a
deliberately FAILING test: the workload is "make pytest pass". This script
stages that workload the way the native API would receive it: an offline
environment plan plus a dry-run exec request for `python -m pytest -q`.
Nothing is executed; runtime execution requires a runtime worker that does
not exist yet.
"""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent


def lsbx(*args):
    proc = subprocess.run([sys.executable, str(ROOT / 'tools' / 'lsbx.py'), '--json', *args],
                          capture_output=True, text=True, timeout=60)
    if proc.returncode != 0:
        raise SystemExit(f'lsbx {" ".join(args)} failed: {proc.stderr.strip()}')
    return json.loads(proc.stdout)


def main():
    fixture = sorted(p.name for p in (HERE / 'workspace').iterdir()
                     if p.name != '__pycache__')
    print('[1/3] workspace fixture (failing test by design):', fixture)

    plan = lsbx('env', 'plan', '--file', str(HERE / 'environment.json'))
    print('[2/3] offline environment plan:')
    print(json.dumps({'plan_digest': plan['plan_digest'],
                      'execution_ready': plan['execution_ready'],
                      'unresolved': plan['unresolved'],
                      'executed': plan['executed']}))

    preview = lsbx('exec', '--generation', '1', '--cwd', '/workspace', '--dry-run',
                   'sb_workspace', '--', 'python', '-m', 'pytest', '-q')
    print('[3/3] dry-run exec request for the pytest verifier command:')
    print(json.dumps(preview))

    if preview['executed'] is not False or plan['executed'] is not False:
        raise SystemExit('a dry run reported executed=true; refusing to present a fake run')
    print('planning-only: pytest never ran here; runtime execution requires a '
          'runtime worker — not yet available')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
