# Codebase Map

One page answering "where does X live?" for new contributors. For setup see
[setup.md](setup.md); for suites see [testing.md](testing.md).

## Runtime code (Rust workspace)

| Area | Path | What's there |
| ---- | ---- | ------------ |
| Domain model, contract types, admission | `crates/ls-core` | `domain`, `contracts`, `admission`; depends on no in-repo crate |
| API surface | `crates/ls-api` | `commands`, `streams`, lifecycle; may depend on `ls-core` |
| Host implementation | `crates/ls-host` | `runtime`, `exec`, `rootfs`, `images`, `resources`, `streams`; may depend on `ls-core` |
| Guest init | `crates/ls-guest` | `init`; the binary that runs as the guest's supervisor/init |

Dependency direction and allowed edges are enforced by
`./dev/check structure`; benchmark-domain vocabulary is lint-forbidden in the
runtime crates.

## Contracts and specifications

| Area | Path |
| ---- | ---- |
| Frozen native v1 contracts (JSON schemas, OpenAPI, state machine, surface, resolution rules) | `contracts/v1` |
| Task/test/feature/requirement catalogs, upstream locks, host profiles | `specs` |
| Internal design documents (authoritative architecture; internal language) | `v5` |
| Architecture decision records | `adr` |

`contracts/v1` is frozen: changing it requires owner/review alignment, not a
drive-by edit.

## Clients and tooling

| Area | Path |
| ---- | ---- |
| Native v1 reference client | `client/python` |
| Development helpers (`lightsandbox_dev`: host probe, capability report) | `python/src` |
| CLI entry (`lsbx`: strict offline planning, dry-run honest about `executed=false`) | `tools/lsbx.py` |
| Pack integrity / task picking / release gate | `tools/packctl.py` |
| Check harness (suites, exit codes) | `dev/check`, `dev/checklib.py` |
| Authorized runtime worker registry (+ template) | `dev/runtime-workers.json`, `dev/runtime-workers.example.json` |

## Tests

| Area | Path |
| ---- | ---- |
| Rust unit tests | inline in each crate (`cargo test --workspace`) |
| Harness self-tests | `tests/harness` |
| Contract lock tests / native contract tests | `tests/contracts`, `tests/native` |
| Pack tool tests | `tests/pack` |
| Python dev-helper tests | `python/tests` |
| Worker-gated areas (runtime, security, rootfs, images) | `tests/runtime`, `tests/security`, `tests/rootfs`, `tests/images` |

## Process artifacts

| Area | Path |
| ---- | ---- |
| Task cards (all / currently worked) | `plans/tasks`, `plans/active` |
| Plan/handoff/incident/review templates | `plans/templates` |
| Round progress notes | `progress` |
| Independent review reports | `reviews` |
| Raw evidence (logs, hashes, host reports) | `evidence` |
| Task state machine (who is verified) | `state/project-status.json` |
| Session handoff entry point | `launch/CONTINUE.md` |

## Reference-only areas (read, don't build on)

- `spike` — **feasibility research** (e.g. the portable-rootless
  runtime spike, the UML kernel spike). Findings inform ADRs and task cards;
  spike code is *never merged into the runtime core* and is not product code.
  `spike/uml/work` is derived build scratch, excluded from pack checks.
- `seed` — older internal protocols kept as local-logic
  reference; the current contract is `contracts/v1`, not the seeds.
- `history` — superseded specifications preserved for audit;
  no normative weight.
- `examples` — planning examples; sample images use
  un-pullable `example.invalid` addresses and are not runtime assets.

## Boundaries worth knowing

- No Trial/Agent/Verifier/Reward concepts in the runtime core (enforced).
- Launch mode and security policy are independent axes.
- Per-sandbox lightweight supervisor; no automatic replay of lost execution
  results; closing an API connection never destroys guest resources.
- The goal is Benchmark Task Parity against the host capability tiers
  (PORTABLE_ROOTLESS / FULL_ROOTLESS / ROOTFUL), not general Docker semantic
  parity — capability degradations must go through the explicit
  compatibility policy, never silent flattening
  (`adr/0001-portable-rootless-runtime.md`).
