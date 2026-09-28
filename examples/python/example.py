#!/usr/bin/env python3
"""python: typed reference-client surface on an offline planning walk.

Demonstrates SandboxRef / ExecSpec / ExitStatus, resolve_launch image-mode
resolution, environment validation, and `lsbx env plan` on the checked-in
environment.json. Planning level: no sandbox daemon ships in this repo.
"""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT / 'client' / 'python' / 'src'))

from lightsandbox.contracts import (ContractError, ExecSpec, ExitStatus,  # noqa: E402
                                    SandboxRef, resolve_launch, validate_environment)


def main():
    print('[1/5] typed contract objects:')
    ref = SandboxRef('sb_demo', 1)
    spec = ExecSpec(argv=['python3', '-c', 'print(2 + 2)'], cwd='/work',
                    env={'TZ': 'UTC'}, timeout_ms=30000)
    print('SandboxRef:', json.dumps(ref.to_dict()))
    print('ExecSpec:', json.dumps(spec.to_dict()))
    status = ExitStatus(kind='exited', exit_code=0)
    print('ExitStatus normalized exit code:', status.normalized_code())

    print('[2/5] launch resolution (image mode), from an OCI-style image config:')
    image = {'Entrypoint': ['/usr/bin/python3'], 'Cmd': ['--version'], 'WorkingDir': '',
             'User': '', 'Env': ['PYTHONUNBUFFERED=1']}
    print(json.dumps(resolve_launch(image)))
    idle = resolve_launch(image, mode='idle-agent')
    print('idle-agent mode argv (image argv parked aside):', json.dumps(idle['argv']))

    print('[3/5] contract boundary: argv and shell are mutually exclusive:')
    try:
        ExecSpec(argv=['true'], shell='true', shell_executable='/bin/sh')
    except ContractError as exc:
        print('rejected as expected:', exc)
    else:
        raise SystemExit('ExecSpec accepted both command forms; the contract has drifted')

    print('[4/5] environment.json validated by the offline planning subset:')
    env = validate_environment(json.loads((HERE / 'environment.json').read_text(encoding='utf-8')))
    print('services:', sorted(env['services']))

    print('[5/5] offline environment plan (tools/lsbx.py env plan):')
    proc = subprocess.run([sys.executable, str(ROOT / 'tools' / 'lsbx.py'), '--json',
                           'env', 'plan', '--file', str(HERE / 'environment.json')],
                          capture_output=True, text=True, timeout=60)
    if proc.returncode != 0:
        raise SystemExit(f'env plan failed: {proc.stderr.strip()}')
    plan = json.loads(proc.stdout)
    print(json.dumps({'plan_digest': plan['plan_digest'],
                      'execution_ready': plan['execution_ready'],
                      'unresolved': plan['unresolved'],
                      'executed': plan['executed']}))
    if plan['executed'] is not False or plan['execution_ready'] is not False:
        raise SystemExit('offline plan claimed readiness or execution; refusing a fake run')
    print('planning-only: nothing was executed; see README.md for what a live daemon would add')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
