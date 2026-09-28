#!/usr/bin/env python3
"""Generate docs/reference/errors.md from the error-code sources of truth.

Sources:
* crates/ls-api/src/lib.rs — the stable NativeErrorCode wire vocabulary
  (variants + as_str + http_status mappings), parsed from Rust source.
* client/python/src/lightsandbox/client.py — literal client-side codes the
  reference client raises when no server error object applies.
* contracts/v1/resolution-rules.json — contract-level condition codes and
  their HTTP status, loaded as data.

Regeneration is byte-exact: the checked-in file must equal generator output.
`--check` compares without writing and fails (exit 1, with a diff) on drift.
Any parse expectation that stops holding aborts the generator loudly instead
of emitting a stale-looking document. Stdlib only.

Exit codes: 0 written/verified, 1 drift/missing/parse failure, 2 usage.
"""
import difflib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LS_API = ROOT / 'crates' / 'ls-api' / 'src' / 'lib.rs'
CLIENT = ROOT / 'client' / 'python' / 'src' / 'lightsandbox' / 'client.py'
RULES = ROOT / 'contracts' / 'v1' / 'resolution-rules.json'
OUTPUT = ROOT / 'docs' / 'reference' / 'errors.md'

# Meanings are prose about code semantics; every parsed code MUST have one
# and unknown keys are rejected, so a changed source forces a doc decision.
SERVER_MEANINGS = {
    'INVALID_ARGUMENT':
        'The request violates the v1 contract (bad identifier, missing or duplicate '
        'command form, non-positive resource value, oversize input). Fix the request; '
        'replaying it verbatim will fail again.',
    'NOT_FOUND':
        'The addressed resource does not exist. A stale generation on an otherwise '
        'valid id is also a lookup failure, not a success.',
    'CONFLICT':
        'The request conflicts with current resource state (for example a generation '
        'mismatch or an illegal lifecycle transition). Re-read state before retrying.',
    'PRECONDITION_FAILED':
        'A host capability or policy precondition is unmet; the request is rejected '
        'before any resource is created. Nothing is silently downgraded.',
    'UNAVAILABLE':
        'The addressed backend or capability is unavailable on this host right now.',
    'INTERNAL':
        'An internal server fault. Report it with the returned request id.',
}
CLIENT_MEANINGS = {
    'TRANSPORT_ERROR': (
        'client',
        'A GET/HEAD request could not reach the API transport. Non-GET requests never '
        'surface this — a lost response is REQUEST_OUTCOME_UNKNOWN instead.'),
    'PROTOCOL_ERROR': (
        'client',
        'The response was not the expected JSON object, or an operation error had an '
        'unexpected shape.'),
    'HTTP_ERROR': (
        'client fallback',
        'HTTP failure without a structured error code in the response body.'),
    'OPERATION_FAILED': (
        'operation',
        'The operation reached status `failed`. This is the default code when the '
        'server error object carries a message but no code.'),
    'REQUEST_OUTCOME_UNKNOWN': (
        'client',
        'A non-GET response was lost in transit. The original request must be queried '
        'by idempotency key before any resubmission — the client never retries implicitly.'),
    'WAIT_TIMEOUT': (
        'client',
        'The client wait deadline elapsed while polling an operation. The server-side '
        'operation was NOT cancelled by the timeout.'),
    'OPERATION_AMBIGUOUS': (
        'client',
        'The operation reported status `unknown`; it requires server-side '
        'reconciliation before its outcome can be trusted.'),
}
CLIENT_RAISED_BY = {
    'HTTP_ERROR': 'ApiError', 'PROTOCOL_ERROR': 'ApiError', 'OPERATION_FAILED': 'ApiError',
    'TRANSPORT_ERROR': 'ApiError', 'REQUEST_OUTCOME_UNKNOWN': 'AmbiguousRequest',
    'WAIT_TIMEOUT': 'WaitTimeout', 'OPERATION_AMBIGUOUS': 'OperationUnknown',
}


def fail(msg):
    raise SystemExit(f'gen_error_docs: parse/source failure: {msg}')


def parse_ls_api():
    source = LS_API.read_text(encoding='utf-8')
    enum = re.search(r'pub enum NativeErrorCode \{(.*?)\}', source, re.S)
    if not enum:
        fail('NativeErrorCode enum not found in ls-api lib.rs')
    variants = re.findall(r'^\s*([A-Z]\w+),?\s*$', enum.group(1), re.M)
    if not variants:
        fail('no NativeErrorCode variants parsed')
    wires = dict(re.findall(r'NativeErrorCode::(\w+) => "([A-Z_]+)"', source))
    statuses = {k: int(v) for k, v in re.findall(r'NativeErrorCode::(\w+) => (\d+)', source)}
    if set(variants) != set(wires) or set(variants) != set(statuses):
        fail(f'enum/as_str/http_status sets diverge: {variants} vs '
             f'{sorted(wires)} vs {sorted(statuses)}')
    if set(wires.values()) != set(SERVER_MEANINGS):
        fail(f'server meanings cover {sorted(SERVER_MEANINGS)} but wire codes are '
             f'{sorted(wires.values())}; update SERVER_MEANINGS deliberately')
    return [(v, wires[v], statuses[v]) for v in variants]


def parse_client_codes():
    source = CLIENT.read_text(encoding='utf-8')
    codes = set(re.findall(r"super\(\)\.__init__\('([A-Z_]+)'", source))
    codes |= set(re.findall(r"ApiError\('([A-Z_]+)'", source))
    codes |= set(re.findall(r"\.get\('code',\s*'([A-Z_]+)'\)", source))
    if not codes:
        fail('no client-side codes parsed from client.py')
    if codes != set(CLIENT_MEANINGS):
        fail(f'client codes {sorted(codes)} != documented {sorted(CLIENT_MEANINGS)}; '
             'update CLIENT_MEANINGS deliberately')
    return sorted(codes)


def parse_condition_codes():
    rules = json.loads(RULES.read_text(encoding='utf-8'))
    try:
        entries = [
            (rules['rejection']['code'], int(rules['rejection']['http_status']),
             'A `true` requirement whose mapped host capability is not SUPPORTED; '
             'rejected before create with no fallback rewriting.'),
            (rules['policy_rejection']['code'], int(rules['policy_rejection']['http_status']),
             'The single-ID compatibility policy (STRICT by default) refuses unfaithful '
             'UID/GID expressions at create/preflight.'),
        ]
    except (KeyError, TypeError, ValueError) as exc:
        fail(f'resolution-rules.json lost rejection/policy_rejection codes: {exc}')
    return entries


def render(server, client_codes, conditions):
    lines = [
        '<!-- GENERATED by tools/gen_error_docs.py. Do not edit by hand: change the',
        '     sources (crates/ls-api, client/python, contracts/v1) and regenerate.',
        '     Drift is checked by `dev/check docs`. -->',
        '',
        '# Error codes',
        '',
        'Status: v1 reference client — pre-release; no sandbox server is bundled in this',
        'repository. The codes below are the planned wire contract: the server table is',
        'the stable shared vocabulary, the condition table binds contract-level',
        'rejections, and the client table lists codes the reference client raises',
        'locally when no server error object applies.',
        '',
        '## Stable wire codes (server)',
        '',
        'These wire strings are part of the cross-language contract and must not drift.',
        '',
        '| Rust variant (`NativeErrorCode`) | Wire code | HTTP status | Meaning |',
        '|---|---|---|---|',
    ]
    for variant, wire, status in server:
        lines.append(f'| `{variant}` | `{wire}` | {status} | {SERVER_MEANINGS[wire]} |')
    lines += [
        '',
        '## Contract-level condition codes (`contracts/v1/resolution-rules.json`)',
        '',
        'Requirement/policy rejections carry their own condition codes on top of the',
        'HTTP status:',
        '',
        '| Code | HTTP status | Meaning |',
        '|---|---|---|',
    ]
    for code, status, meaning in conditions:
        lines.append(f'| `{code}` | {status} | {meaning} |')
    lines += [
        '',
        '## Reference-client codes',
        '',
        'Raised by the client itself (or defaulted when a response lacks a structured',
        'code). They never indicate success and are never retried implicitly.',
        '',
        '| Code | Raised as / by | Meaning |',
        '|---|---|---|',
    ]
    for code in client_codes:
        label, meaning = CLIENT_MEANINGS[code]
        lines.append(f'| `{code}` | `{CLIENT_RAISED_BY[code]}` ({label}) | {meaning} |')
    lines += [
        '',
        '## CLI exit codes',
        '',
        'The `lsbx` CLI maps the above onto process exit codes for scripting:',
        '',
        '| Exit | When |',
        '|---|---|',
        '| 0 | Success, including dry-run previews (`\"executed\": false`). |',
        '| 2 | Usage or client-side contract error (`INVALID_ARGUMENT`). |',
        '| 8 | `WAIT_TIMEOUT` — the wait deadline elapsed; the operation still exists server-side. |',
        '| 10 | `REQUEST_OUTCOME_UNKNOWN` or `OPERATION_AMBIGUOUS` — outcome must be reconciled before resubmission. |',
        '| 11 | Any other API error (including `INVALID_ARGUMENT` returned by a server). |',
        '',
    ]
    return '\n'.join(lines)


def main(argv):
    if [a for a in argv if a != '--check']:
        print(__doc__, file=sys.stderr)
        return 2
    server = parse_ls_api()
    client_codes = parse_client_codes()
    conditions = parse_condition_codes()
    text = render(server, client_codes, conditions)
    if '--check' not in argv:
        OUTPUT.write_text(text, encoding='utf-8')
        print(f'wrote {OUTPUT.relative_to(ROOT)}: {len(server)} error codes, '
              f'{len(client_codes)} client codes, {len(conditions)} condition codes')
        return 0
    if not OUTPUT.is_file():
        print(f'{OUTPUT.relative_to(ROOT)} missing; run: python3 tools/gen_error_docs.py')
        return 1
    current = OUTPUT.read_text(encoding='utf-8')
    if current != text:
        print(f'DRIFT: {OUTPUT.relative_to(ROOT)} differs from regenerated content; '
              'run: python3 tools/gen_error_docs.py')
        diff = difflib.unified_diff(current.splitlines(), text.splitlines(),
                                    str(OUTPUT.relative_to(ROOT)), 'regenerated', lineterm='')
        print('\n'.join(list(diff)[:80]))
        return 1
    print(f'verified {len(server)} error codes (+{len(client_codes)} client codes, '
          f'{len(conditions)} condition codes); {OUTPUT.relative_to(ROOT)} is drift-free')
    return 0


if __name__ == '__main__':
    raise SystemExit(main(sys.argv[1:]))
