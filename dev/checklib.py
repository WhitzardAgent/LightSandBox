#!/usr/bin/env python3
"""LightSandbox dev/check runner core (T00).

Contract (docs/harness/04_TEST_RUNNER_AND_EVIDENCE.md, plans/tasks/T00.md):

* argv only, never shell strings.
* unknown suite              -> exit 2 with the suite list
* zero collected tests       -> exit 1 ("zero tests collected")
* missing tool prerequisite  -> exit 4 and names the tool
* runtime/security/... suites without an authorized worker -> exit 3 BLOCKED
* child failure propagates with its raw exit code; nothing is converted to PASS.

Exit codes: 0 ok, 1 failed, 2 usage, 3 blocked, 4 missing prerequisite.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - python < 3.11 guard
    tomllib = None

ROOT = Path(__file__).resolve().parents[1]

EXIT_OK = 0
EXIT_FAIL = 1
EXIT_USAGE = 2
EXIT_BLOCKED = 3
EXIT_PREREQ = 4

RUN_DIR = ROOT / 'dev' / '.check-runs'

CRATE_NAMES = ('ls-core', 'ls-host', 'ls-api', 'ls-guest')
ALLOWED_EDGES = {
    'ls-core': set(),
    'ls-host': {'ls-core'},
    'ls-api': {'ls-core'},
    'ls-guest': {'ls-core'},
}
FORBIDDEN_VOCABULARY = re.compile(r'\b(Trial|Verifier|Reward|Evaluation)\b')


def run_command(argv, cwd=None, env=None, timeout=1200):
    """Run one argv command, capture output, preserve the raw exit code."""
    start = time.monotonic()
    proc = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, text=True,
                          timeout=timeout, errors='replace')
    duration_ms = int((time.monotonic() - start) * 1000)
    output = proc.stdout + proc.stderr
    return {
        'argv': [str(a) for a in argv],
        'exit': proc.returncode,
        'duration_ms': duration_ms,
        'output': output,
    }


def _which(tool, env):
    return shutil.which(tool, path=(env or os.environ).get('PATH'))


def _toolchain_pin():
    data = json.loads((ROOT / 'dev' / 'toolchain.json').read_text())
    return data['rustc']


def cargo_test(root, env=None):
    """`cargo test --workspace`; returns collected count and exact status."""
    cargo = _which('cargo', env)
    if cargo is None:
        return {'argv': ['cargo', 'test', '--workspace'], 'exit': EXIT_PREREQ,
                'reason': 'missing prerequisite: cargo', 'tests': 0}
    rustc = _which('rustc', env)
    if rustc is None:
        return {'argv': ['rustc', '--version'], 'exit': EXIT_PREREQ,
                'reason': 'missing prerequisite: rustc', 'tests': 0}
    version = subprocess.run([rustc, '--version'], capture_output=True, text=True, env=env).stdout
    pin = _toolchain_pin()
    if pin not in version:
        return {'argv': [rustc, '--version'], 'exit': EXIT_PREREQ,
                'reason': f'pinned toolchain {pin} not satisfied by: {version.strip()}', 'tests': 0}
    res = run_command([cargo, 'test', '--workspace', '--quiet'], cwd=root, env=env)
    tests = sum(int(m) for m in re.findall(r'test result: .*?(\d+) passed', res['output']))
    out = {'argv': res['argv'], 'exit': res['exit'], 'tests': tests,
           'duration_ms': res['duration_ms'], 'output': res['output']}
    if res['exit'] == 0 and tests == 0:
        out['exit'] = EXIT_FAIL
        out['reason'] = 'zero tests collected'
    return out


def python_unittest(start_dir, top_dir=None, patterns=None, label=None):
    """unittest discovery; zero collected tests converts to failure."""
    start_dir = Path(start_dir)
    top_dir = Path(top_dir) if top_dir else start_dir
    argv = [sys.executable, '-m', 'unittest', 'discover', '-s', str(start_dir), '-t', str(top_dir)]
    for pat in (patterns or []):
        argv += ['-p', pat]
    res = run_command(argv)
    match = re.search(r'Ran (\d+) test', res['output'])
    tests = int(match.group(1)) if match else 0
    out = {'argv': res['argv'], 'label': label or start_dir.name,
           'exit': res['exit'], 'tests': tests,
           'duration_ms': res['duration_ms'], 'output': res['output']}
    if tests == 0:
        # T00-C01: zero collected tests is always a failure, no matter how the
        # interpreter exited (unittest exits 5 when nothing is discovered).
        out['raw_exit'] = res['exit']
        out['exit'] = EXIT_FAIL
        out['reason'] = 'zero tests collected'
    return out


def python_script(path, label):
    res = run_command([sys.executable, str(path)])
    return {'argv': res['argv'], 'label': label, 'exit': res['exit'], 'tests': 0,
            'duration_ms': res['duration_ms'], 'output': res['output']}


# ---------------------------------------------------------------- suites

def suite_unit(root):
    commands = [
        cargo_test(root),
        python_unittest(root / 'tests' / 'harness', root, label='harness-unit',
                        patterns=['test_run_contract.py', 'test_structure_lint.py']),
        python_unittest(root / 'python' / 'tests', root / 'python', label='python-dev'),
    ]
    return commands


def suite_fast(root):
    commands = suite_unit(root)
    commands.append(python_unittest(root / 'tests' / 'harness', root,
                                    label='harness-all', patterns=['test_*.py']))
    commands.append(python_script(root / 'dev' / 'check_native.py', 'native-reference'))
    commands.append(python_script(root / 'dev' / 'check_seeds.py', 'seed-reference'))
    commands.append(python_unittest(root / 'tests' / 'pack', root, label='pack-tools'))
    res = run_command([sys.executable, str(root / 'tools' / 'packctl.py'), 'validate'])
    commands.append({'argv': res['argv'], 'label': 'packctl-validate', 'exit': res['exit'],
                     'tests': 0, 'duration_ms': res['duration_ms'], 'output': res['output']})
    return commands


def lint_structure(root):
    """Static structure rules. Findings: {rule, path, detail}. Exit 1 when any."""
    if tomllib is None:
        return [{'rule': 'prereq', 'path': 'dev/checklib.py',
                 'detail': 'python >= 3.11 with tomllib is required for the structure lint'}]
    findings = []
    root = Path(root)

    def manifest(path):
        return tomllib.loads(path.read_text(encoding='utf-8'))

    root_manifest = manifest(root / 'Cargo.toml')
    members = set(root_manifest.get('workspace', {}).get('members', []))
    # Only dependencies that actually resolve to in-repo crates are direction
    # edges: a `workspace = true` dep whose workspace definition is a registry
    # crate (serde, ...) is not an inter-crate edge.
    workspace_internal = {
        name for name, spec in (root_manifest.get('workspace', {}).get('dependencies') or {}).items()
        if isinstance(spec, dict) and 'path' in spec
    }
    expected = {f'crates/{name}' for name in CRATE_NAMES}
    if members != expected:
        findings.append({'rule': 'workspace-members', 'path': 'Cargo.toml',
                         'detail': f'workspace members {sorted(members)} != expected {sorted(expected)}'})

    crates_dir = root / 'crates'
    for entry in sorted(crates_dir.iterdir()) if crates_dir.is_dir() else []:
        if entry.is_dir() and entry.name not in CRATE_NAMES:
            findings.append({'rule': 'unknown-crate', 'path': f'crates/{entry.name}',
                             'detail': 'crate directory not in CRATE_NAMES contract'})

    for name in CRATE_NAMES:
        manifest_path = crates_dir / name / 'Cargo.toml'
        if not manifest_path.exists():
            findings.append({'rule': 'missing-crate', 'path': f'crates/{name}/Cargo.toml',
                             'detail': 'expected crate manifest missing'})
            continue
        data = manifest(manifest_path)
        rel = f'crates/{name}/Cargo.toml'
        dep_sections = ['dependencies', 'dev-dependencies', 'build-dependencies']
        for section in dep_sections:
            for dep, spec in (data.get(section) or {}).items():
                if not isinstance(spec, dict):
                    continue
                is_internal = ('path' in spec
                               or (spec.get('workspace') is True and dep in workspace_internal))
                if not is_internal:
                    continue
                if dep not in ALLOWED_EDGES.get(name, set()):
                    findings.append({'rule': 'dependency-direction', 'path': rel,
                                     'detail': f'{name} must not depend on {dep!r} '
                                               f'(allowed: {sorted(ALLOWED_EDGES[name]) or "none"})'})

    for rs in sorted(crates_dir.rglob('*.rs')):
        if 'target' in rs.parts:
            continue
        rel = rs.relative_to(root).as_posix()
        text = rs.read_text(encoding='utf-8', errors='replace')
        hit = FORBIDDEN_VOCABULARY.search(text)
        if hit:
            findings.append({'rule': 'forbidden-vocabulary', 'path': rel,
                             'detail': f'bare evaluation-domain term {hit.group(1)!r} in runtime crate'})

    def scan_verdict(base, suffixes, needle):
        base_path = root / base
        if not base_path.is_dir():
            return
        for path in sorted(base_path.rglob('*')):
            if path.suffix not in suffixes or not path.is_file():
                continue
            if '__pycache__' in path.parts or 'target' in path.parts:
                continue
            text = path.read_text(encoding='utf-8', errors='replace')
            if needle in text:
                findings.append({'rule': 'verdict-protection',
                                 'path': path.relative_to(root).as_posix(),
                                 'detail': f'skip marker {needle!r} weakens the verdict'})

    # Verdict protection covers the product test surfaces owned by the T00
    # harness. tests/pack is the pre-T00 pack/reference layer: it still carries
    # a legacy environment-gated `skipUnless(shutil.which("git"))`, recorded as
    # known debt in plans/active/T00.md instead of being silently rewritten.
    scan_verdict('crates', {'.rs'}, '#[ignore')
    scan_verdict('tests/harness', {'.py'}, 'unittest.skip')
    scan_verdict('python/tests', {'.py'}, 'unittest.skip')
    return findings


def _load_workers(root):
    registry = root / 'dev' / 'runtime-workers.json'
    if not registry.exists():
        return {}
    return json.loads(registry.read_text(encoding='utf-8'))


def _blocked(reason):
    return {'argv': [], 'exit': EXIT_BLOCKED, 'reason': reason, 'tests': 0}


def suite_worker_gated(root, kind, task, worker):
    if not worker:
        return [_blocked(f'{kind} suite requires --worker WORKER_ID of an authorized worker')]
    workers = _load_workers(root)
    entry = workers.get(worker)
    if not entry or not entry.get('authorized'):
        return [_blocked(f'worker {worker!r} is not registered/authorized in dev/runtime-workers.json')]
    return [_blocked(f'worker {worker!r} registered, but the {kind} adapter is not implemented in T00; '
                     f'no runtime commands were executed')]


def suite_parity(root, benchmark, manifest):
    if not benchmark or benchmark not in ('swebench', 'terminalbench', 'cybergym'):
        return [_blocked('parity requires --benchmark swebench|terminalbench|cybergym')]
    if not manifest:
        return [_blocked('parity requires --manifest LOCKED_MANIFEST')]
    path = Path(manifest)
    if not path.exists():
        return [_blocked(f'benchmark manifest not found: {manifest}')]
    try:
        status = json.loads(path.read_text(encoding='utf-8')).get('status')
    except json.JSONDecodeError:
        return [_blocked(f'benchmark manifest is not valid JSON: {manifest}')]
    if status != 'LOCKED':
        return [_blocked(f'benchmark manifest status is {status!r}, not LOCKED')]
    return [_blocked('manifest LOCKED but the parity runner is not implemented in T00')]


def suite_performance(root, profile):
    if not profile:
        return [_blocked('performance requires --profile LOCKED_PROFILE')]
    path = root / 'dev' / 'profiles' / f'{profile}.json'
    if not path.exists():
        return [_blocked(f'no locked performance profile at dev/profiles/{profile}.json')]
    return [_blocked('performance runner is not implemented in T00')]


def _suite_defs():
    return {
        'unit': lambda root, opts: suite_unit(root),
        'fast': lambda root, opts: suite_fast(root),
        'structure': lambda root, opts: [],
        'contracts': lambda root, opts: [
            python_unittest(root / 'tests' / 'native', root, label='native-contracts',
                            patterns=['test_contracts.py', 'test_schema.py']),
        ],
        'runtime': lambda root, opts: suite_worker_gated(root, 'runtime', opts.get('task'), opts.get('worker')),
        'differential': lambda root, opts: suite_worker_gated(root, 'differential', opts.get('task'), opts.get('worker')),
        'security': lambda root, opts: suite_worker_gated(root, 'security', opts.get('task'), opts.get('worker')),
        'parity': lambda root, opts: suite_parity(root, opts.get('benchmark'), opts.get('manifest')),
        'performance': lambda root, opts: suite_performance(root, opts.get('profile')),
        'selftest-exit17': lambda root, opts: [
            python_script(root / 'tests' / 'harness' / 'fixtures' / 'exit17.py', 'exit17')],
    }


PUBLIC_SUITES = ('unit', 'fast', 'structure', 'contracts', 'runtime',
                 'differential', 'security', 'parity', 'performance')
TEST_COLLECTING = ('unit', 'fast', 'contracts')


def _git_state(root):
    def git(*args):
        proc = subprocess.run(['git', '-C', str(root), *args],
                              capture_output=True, text=True)
        return proc.stdout.strip() if proc.returncode == 0 else None
    commit = git('rev-parse', 'HEAD')
    status = git('status', '--porcelain')
    dirty = bool(status) if status is not None else None
    return {'commit': commit, 'dirty': dirty}


def run_suite(name, root, **opts):
    """Execute one suite and summarize. Never raises for ordinary failures."""
    root = Path(root)
    started = datetime.now(timezone.utc).isoformat()
    if name not in _suite_defs():
        return {'suite': name, 'exit': EXIT_USAGE,
                'reason': f"unknown suite: {name}; available: {', '.join(PUBLIC_SUITES)}",
                'commands': [], 'tests_total': 0, 'failures': 0, 'blocked': 0}
    if name == 'contracts' and not opts.get('task'):
        return {'suite': name, 'exit': EXIT_USAGE,
                'reason': 'contracts requires --task TASK_ID', 'commands': [],
                'tests_total': 0, 'failures': 0, 'blocked': 0}

    findings = []
    if name == 'structure':
        findings = lint_structure(root)
        commands = [{'argv': ['dev/check', 'structure'], 'exit': EXIT_FAIL if findings else EXIT_OK,
                     'tests': 0, 'output': json.dumps(findings, indent=2)}]
    else:
        commands = _suite_defs()[name](root, opts)

    prereq = sum(1 for c in commands if c['exit'] == EXIT_PREREQ)
    blocked = sum(1 for c in commands if c['exit'] == EXIT_BLOCKED)
    tests_total = sum(c.get('tests', 0) for c in commands)
    hard_fail = sum(1 for c in commands if c['exit'] not in (EXIT_OK, EXIT_BLOCKED, EXIT_PREREQ))
    # Every hard failure counts, regardless of the raw child exit code (exit 17
    # is one failed command, not merely a non-1 exit to display).
    failures = hard_fail

    exit_code = EXIT_OK
    reason = ''
    if prereq:
        exit_code, reason = EXIT_PREREQ, next(c['reason'] for c in commands if c['exit'] == EXIT_PREREQ)
    elif blocked and hard_fail == 0:
        exit_code = EXIT_BLOCKED
        reason = commands[0].get('reason', 'blocked')
    if hard_fail:
        exit_code = EXIT_FAIL
    if name in TEST_COLLECTING and exit_code == EXIT_OK and tests_total == 0:
        exit_code, reason = EXIT_FAIL, 'zero tests collected'

    summary = {
        'suite': name,
        'started_at': started,
        'root': str(root),
        'git': _git_state(root),
        'exit': exit_code,
        'reason': reason,
        'tests_total': tests_total,
        'failures': failures,
        'blocked': blocked,
        'findings': findings,
        'commands': commands,
    }
    _persist_run(summary)
    return summary


def _persist_run(summary):
    try:
        RUN_DIR.mkdir(parents=True, exist_ok=True)
        stamp = summary['started_at'].replace(':', '').replace('+', 'Z')
        base = RUN_DIR / f"{stamp}-{summary['suite']}"
        raw = ''
        for cmd in summary['commands']:
            raw += f"$ {' '.join(cmd['argv'])}\n(exit {cmd['exit']})\n{cmd.get('output', '')}\n\n"
        raw_path = base.with_suffix('.log')
        raw_path.write_text(raw, encoding='utf-8')
        digest = hashlib.sha256(raw.encode()).hexdigest()
        meta = {k: v for k, v in summary.items() if k != 'commands'}
        meta['commands'] = [
            {k: v for k, v in c.items() if k != 'output'} for c in summary['commands']]
        meta['log_path'] = str(raw_path)
        meta['log_sha256'] = digest
        base.with_suffix('.json').write_text(json.dumps(meta, indent=2) + '\n', encoding='utf-8')
    except OSError:
        pass  # logging must never turn a red run green or vice versa


def _print_summary(summary, as_json):
    if not as_json:
        print(f"suite={summary['suite']} exit={summary['exit']} "
              f"tests_total={summary['tests_total']} failures={summary['failures']} "
              f"blocked={summary['blocked']}")
        for cmd in summary['commands']:
            note = f" ({cmd.get('reason')})" if cmd.get('reason') else ''
            print(f"  exit={cmd['exit']:>3} tests={cmd.get('tests', 0):<4} "
                  f"{cmd.get('label') or ' '.join(cmd['argv'])}{note}")
        for finding in summary.get('findings', []):
            print(f"  LINT [{finding['rule']}] {finding['path']}: {finding['detail']}")
        if summary.get('reason'):
            print(f"reason: {summary['reason']}")
    printable = dict(summary)
    printable['commands'] = [
        {k: v for k, v in c.items() if k != 'output'} for c in summary['commands']]
    print(json.dumps(printable, indent=None))


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    opts = {'json': False}
    positionals = []
    it = iter(range(len(argv)))
    skip = False
    for i in it:
        if skip:
            skip = False
            continue
        arg = argv[i]
        if arg == '--json':
            opts['json'] = True
        elif arg.startswith('--') and i + 1 < len(argv):
            opts[arg[2:]] = argv[i + 1]
            skip = True
        elif arg.startswith('--'):
            opts[arg[2:]] = True
        else:
            positionals.append(arg)
    if not positionals:
        print(f"usage: dev/check <{'|'.join(PUBLIC_SUITES)}> [--task T] [--worker W] "
              f"[--benchmark B --manifest M] [--profile P] [--json]", file=sys.stderr)
        return EXIT_USAGE
    summary = run_suite(positionals[0], ROOT, **opts)
    _print_summary(summary, opts['json'])
    if summary['exit'] == EXIT_USAGE and 'reason' in summary:
        print(summary['reason'], file=sys.stderr)
    return summary['exit']


if __name__ == '__main__':
    raise SystemExit(main())
