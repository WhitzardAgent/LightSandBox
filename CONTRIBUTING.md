# Contributing to LightSandbox

Thank you for your interest. LightSandbox is an independent, KVM-free Linux
sandbox project: a Rust runtime, a Python SDK/CLI, frozen native v1 contracts,
and an optional Harbor adapter.

Before anything else, understand the current state: this repository contains
a design pack with **tested reference logic and clients, not a running
sandbox**. No mock, dry-run, or plan-mode output proves isolation, SDK
interoperability, or benchmark parity (see [AGENTS.md](AGENTS.md) and
[README.md](README.md)). Contributions that blur this line will be rejected.

## Development setup

See [docs/development/setup.md](docs/development/setup.md) for the full guide.
Short version:

- **Python ≥ 3.11** with `httpx` and `jsonschema` at the versions recorded in
  [dev/requirements-reference.txt](dev/requirements-reference.txt).
- **Rust 1.98.1** via rustup (pinned in [dev/toolchain.json](dev/toolchain.json);
  the pin is verified before `cargo test` runs).
- **No Docker required** for the unit gates. Runtime, differential and
  security suites need an authorized test worker; without one they report
  `BLOCKED`, which is an honest result — do not work around it.

## Code organization

[docs/development/codebase-map.md](docs/development/codebase-map.md) is the
one-page "where does X live" guide. In brief:

- `crates/ls-core` — domain model and contract types; depends on nothing.
- `crates/ls-api` — API surface, lifecycle, commands and streams.
- `crates/ls-host` — host-side implementation (runtime, exec, rootfs, images,
  resources, streams).
- `crates/ls-guest` — guest init (`init`), which becomes PID 1 inside the guest.
- `client/python` — native-v1 reference client.
- `python/` — development helpers (`lightsandbox_dev`, e.g. host probing).
- `contracts/v1` — frozen native v1 contracts (schemas, OpenAPI, state machine,
  surface). Frozen means changes need a designated owner and review.
- `specs/` — task/test/feature/requirement catalogs and locks.
- `tests/` — `unit`, `harness`, `contracts`, `native`, `pack`, plus
  worker-gated `runtime`, `security`, `rootfs`, `images` areas.
- `spike/` — feasibility research only; never merged into the runtime core.
- `seed/` — older internal protocols kept as local reference; not the current
  contract.

Dependency direction is enforced by the structure lint: `ls-core` depends on
no in-repo crate; `ls-api`, `ls-host`, `ls-guest` may depend on `ls-core`
only. Benchmark vocabulary (`Trial`, `Verifier`, `Reward`, `Evaluation`) is
lint-forbidden inside the runtime crates.

## How work is organized

Work flows through task cards, not open-ended pull requests:

1. The task catalog lives in [specs/task-catalog.json](specs/task-catalog.json);
   `python3 tools/packctl.py next` picks a dependency-ready task and
   `python3 tools/packctl.py context T00` loads its card.
2. Task cards live in `plans/tasks/` (all tasks) and `plans/active/`
   (currently worked). Use the templates in `plans/templates/`.
3. Round notes go to `progress/`; independent review reports go to
   `reviews/`; raw evidence goes to `evidence/`.
4. Task state is tracked in `state/project-status.json`. A task reaches
   `verified` **only** when the integrator updates it after trusted CI and an
   independent review by a different responsible person — never because the
   implementer believes it is done.

If you are contributing outside the task system (typo fixes, small docs),
open a normal pull request; the testing and evidence rules below still apply.

## Test-driven development

The expected loop is: failing test → minimal implementation → real
verification → independent review.

- Write the failing test first and keep the real failure output in the
  record, including the exact command and exit code.
- Implement the smallest change that turns that specific test green.
- Verify with the actual suites (below). "Looks reasonable" is not a verdict.
- Never change test criteria, denominators, oracles, verifiers, or security
  profiles to obtain a pass.

## Running the tests

[docs/development/testing.md](docs/development/testing.md) documents every
suite and exit code. The daily commands:

```bash
python3 tools/packctl.py validate      # pack link/catalog integrity
./dev/check fast                       # unit + harness + reference + pack checks
./dev/check unit                       # cargo test --workspace + python unit layers
./dev/check structure                  # static structure lint (dep direction, lint rules)
python3 dev/check_native.py            # native-v1 reference checks
python3 dev/check_seeds.py             # seed reference checks
python3 -m unittest discover -s tests/pack -v
```

Zero collected tests is a failure, not a pass. Suite exit codes: `0` ok,
`1` failed, `2` usage error, `3` blocked, `4` missing prerequisite.

## Commit discipline

- Small, scoped commits; reference the task ID in the message (e.g.
  `T12: add log streaming unit frame parser`).
- Keep the evidence of what you ran (raw logs, exit codes, test counts).
- Never fake success: no claiming a suite passed when it printed BLOCKED, no
  trimming failing cases, no swapping in mocks where real verification was
  required.
- Don't mix unrelated refactors into a task commit.

## Architecture decisions (ADRs)

Architecture decisions are recorded as ADRs in `docs/adr/` with sequential
numbers (`0001-portable-rootless-runtime.md`, `0002-...`). If your change
alters architecture, contracts, or a previously decided trade-off, write the
ADR first and reference it from the change. Do not silently reverse an ADR in
code.

## Evidence rule

Evidence lives in `evidence/` as raw artifacts: command lines, exit codes,
test counts, and logs — not summaries alone. Task-level evidence records
follow [specs/evidence-contract.json](specs/evidence-contract.json) and
reference sha256-hashed raw artifacts. Never rewrite historical evidence to
hide a later change; append new evidence instead.

## Blocked runtime tests

`BLOCKED` is not `skipped`, and neither is a failure. When a
runtime/differential/security suite cannot run (no authorized worker, missing
tool, missing lock):

- Record it as blocked with the reason (which worker/tool/lock is absent) and
  the raw exit code (`3` for blocked, `4` for missing prerequisite).
- Do not substitute a mock, a dry run, or a partial run and present it as the
  real verdict.
- Runtime suites only run against hosts registered in
  `dev/runtime-workers.json` by an authorized operator; see
  [docs/development/setup.md](docs/development/setup.md#authorized-runtime-workers).

## Security issues

Do **not** report vulnerabilities in public issues. Follow
[SECURITY.md](SECURITY.md).

## CONTRIBUTING.md vs AGENTS.md

[AGENTS.md](AGENTS.md) is the operating contract for automated/agent
contributors (execution boundaries, safety rails, check commands). This file
is the guide for human contributors. Where they overlap — no fake success, no
host mutation without an authorized worker, no silent oracle changes — the
rule applies to everyone regardless of who typed the command.
