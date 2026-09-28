#!/usr/bin/env python3
"""hello: the minimal LightSandbox sanity example (planning level).

Prints the client version, builds and validates one ExecSpec through the
reference contracts module, and shows the exact HTTP requests the CLI would
send for `sandbox create` + `exec echo hello` in --dry-run form. No server
ships in this repo, so every printed request has "executed": false.
"""
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'client' / 'python' / 'src'))

from lightsandbox.contracts import ExecSpec  # noqa: E402

IMAGE = 'registry.example.invalid/demo@sha256:' + 'a' * 64


def lsbx(*args):
    proc = subprocess.run([sys.executable, str(ROOT / 'tools' / 'lsbx.py'), '--json', *args],
                          capture_output=True, text=True, timeout=60)
    if proc.returncode != 0:
        raise SystemExit(f'lsbx {" ".join(args)} failed: {proc.stderr.strip()}')
    return json.loads(proc.stdout)


def main():
    version = lsbx('version')
    print('[1/4] client version (tools/lsbx.py version):')
    print(json.dumps(version))

    spec = ExecSpec(argv=['echo', 'hello'])
    print('[2/4] ExecSpec built and validated by lightsandbox.contracts:')
    print(json.dumps(spec.to_dict()))

    create = lsbx('sandbox', 'create', '--image', IMAGE, '--dry-run')
    print('[3/4] dry-run sandbox create for the digest-pinned example image:')
    print(json.dumps(create))

    run = lsbx('exec', '--generation', '1', '--dry-run', 'sb_demo', '--', 'echo', 'hello')
    print('[4/4] dry-run exec request for `echo hello`:')
    print(json.dumps(run))

    if version['runtime_implemented'] is not False:
        raise SystemExit('client reports a runtime; this example expected the pre-release client')
    if create['executed'] is not False or run['executed'] is not False:
        raise SystemExit('a dry run reported executed=true; refusing to present a fake run')
    print('planning-only: nothing was executed (no sandbox server ships in this repo)')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
