#!/usr/bin/env python3
"""capability-routing: requirement sets -> backend plan, with rule citations.

A tested *planning* example: it loads the frozen contract files
(contracts/v1/resolution-rules.json, contracts/v1/sandbox-requirements.schema.json),
the AUTO order from crates/ls-core/src/domain/backend.rs, and the measured
host/spike evidence, then walks two requirement sets through the ADR 0002
AUTO selection rule. It is deliberately drift-unsafe-tolerant: if any of
those sources change shape, this script breaks loudly instead of presenting
stale rules as current. Production selection is the Rust `plan()` in
backend.rs behind the API server, which is not shipped.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RULES_PATH = ROOT / 'contracts' / 'v1' / 'resolution-rules.json'
SCHEMA_PATH = ROOT / 'contracts' / 'v1' / 'sandbox-requirements.schema.json'
BACKEND_RS = ROOT / 'crates' / 'ls-core' / 'src' / 'domain' / 'backend.rs'
HOST_REPORT = ROOT / 'evidence' / 'host' / 'hostreport-v1.json'
UML_BOOT = ROOT / 'spike' / 'results' / 'uml-boot.json'


def drift(msg):
    raise SystemExit(f'contract/evidence drift detected: {msg}')


def load_sources():
    rules = json.loads(RULES_PATH.read_text(encoding='utf-8'))
    schema = json.loads(SCHEMA_PATH.read_text(encoding='utf-8'))
    try:
        schema_dims = list(schema['properties']['requires']['properties'])
    except KeyError:
        drift('sandbox-requirements.schema.json lost properties.requires')
    map_dims = rules.get('requirements_map')
    if not isinstance(map_dims, dict):
        drift('resolution-rules.json lost requirements_map')
    if set(schema_dims) != set(map_dims):
        drift(f'requirement dimensions diverge: schema={sorted(schema_dims)} '
              f'vs requirements_map={sorted(map_dims)}')
    source = BACKEND_RS.read_text(encoding='utf-8')
    match = re.search(r'AUTO_ORDER:\s*\[BackendKind;\s*\d+\]\s*=\s*\[(.*?)\]', source, re.S)
    if not match:
        drift('BackendKind::AUTO_ORDER not found in backend.rs')
    order = [name.upper() for name in re.findall(r'BackendKind::(\w+)', match.group(1))]
    if not order or order[0] != 'NATIVE':
        drift(f'unexpected AUTO order {order}; expected NATIVE first per ADR 0002')
    host = json.loads(HOST_REPORT.read_text(encoding='utf-8'))
    findings = {f['name']: f for f in host.get('findings', [])}
    uml = json.loads(UML_BOOT.read_text(encoding='utf-8'))
    return rules, schema_dims, order, findings, uml


def native_observation(dim, rules, findings):
    """What the measured host observes for the NATIVE backend on `dim`."""
    mapping = rules['requirements_map'][dim]
    if dim != 'multi_uid':
        drift(f'this planning example only models multi_uid on the native backend, got {dim!r}')
    uid = findings.get('uid_mappings')
    if uid is None:
        drift('host report lost the uid_mappings finding')
    excerpt = uid.get('output_excerpt', '')
    if 'single identity' not in excerpt:
        drift(f'uid_mappings evidence no longer records the single-ID map: {excerpt!r}')
    return 'SINGLE_ID', (f"evidence/host/hostreport-v1.json findings.uid_mappings="
                         f"{uid['status']} (`{excerpt.strip()}`)")


def kernel_observation(dim, uml):
    """What the UML spike evidence shows for the KERNEL backend on `dim`."""
    if dim != 'multi_uid':
        drift(f'this planning example only models multi_uid on the kernel backend, got {dim!r}')
    markers = uml.get('markers', {})
    if uml.get('verdict') != 'BOOT_OK' or not markers.get('LSB_CHOWN_OK') or not markers.get('LSB_SU_OK'):
        drift('uml-boot.json no longer records the multi-UID guest markers')
    stat = uml.get('fields', {}).get('LSB_STAT', '?')
    return 'MULTI_ID', (f'spike/results/uml-boot.json verdict=BOOT_OK '
                        f'LSB_CHOWN_OK=true LSB_SU_OK=true LSB_STAT={stat}')


def simulate(name, requirements, rules, order, findings, uml):
    print(f'=== Scenario: {name} ===')
    required = sorted(d for d, on in requirements.items() if on)
    print('requested requires:', json.dumps(requirements))
    print('hard requirement dimensions:', required or '(none)')
    rejection = rules['rejection']
    print(f"AUTO order: {' -> '.join(order)}  "
          '[crates/ls-core/src/domain/backend.rs BackendKind::AUTO_ORDER; ADR 0002 selection rule: '
          'first available backend observing SUPPORTED for every hard requirement]')
    selected = None
    for backend in order:
        if backend == 'NATIVE':
            userns = findings.get('userns', {}).get('status')
            if userns != 'SUPPORTED':
                print(f'NATIVE: skipped — user namespaces not supported on this host ({userns})')
                continue
            missing = []
            for dim in required:
                mapping = rules['requirements_map'][dim]
                observed, evidence = native_observation(dim, rules, findings)
                print(f'rule: {dim} -> host capability {mapping["capability"]} MUST be '
                      f'{mapping["accepted_value"]}  '
                      f'[contracts/v1/resolution-rules.json requirements_map.{dim}]')
                print(f'NATIVE: observed {observed} ({evidence})')
                if observed != mapping['accepted_value']:
                    print(f'NATIVE: requirement NOT satisfiable — rejection code '
                          f'{rejection["code"]} / HTTP {rejection["http_status"]}; '
                          'no silent downgrade, no fallback rewriting')
                    missing.append(dim)
            if missing:
                continue
            selected = backend
            break
        if backend == 'KERNEL':
            missing = []
            for dim in required:
                observed, evidence = kernel_observation(dim, uml)
                print(f'KERNEL: observed {observed} ({evidence})')
                mapping = rules['requirements_map'][dim]
                if observed != mapping['accepted_value']:
                    print('KERNEL: requirement NOT satisfiable on this evidence set')
                    missing.append(dim)
            if missing:
                continue
            selected = backend
            break
        print(f'{backend}: no measured offer in this example\'s evidence set — skipped')
    if selected is None:
        print(f'plan: REJECTED pre-create with {rejection["code"]} '
              f'(HTTP {rejection["http_status"]})')
    else:
        print(f'plan: {selected} (lightest available backend satisfying all '
              f'{len(required)} required dimension(s))')
    print()
    return selected


def main():
    rules, _dims, order, findings, uml = load_sources()
    first = simulate('SandboxSpec.requires = {}', {}, rules, order, findings, uml)
    second = simulate('SandboxSpec.requires = {"multi_uid": true}', {'multi_uid': True},
                      rules, order, findings, uml)
    if first != 'NATIVE':
        drift(f'empty requirements should select NATIVE per AUTO order, got {first}')
    if second != 'KERNEL':
        drift(f'multi_uid=true should select KERNEL on this evidence set, got {second}')
    print('planning-level simulation only: production routing is ls-core domain::backend::plan() '
          'behind the API server (not shipped; see README.md)')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
