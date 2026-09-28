# Testing

All local verification goes through the check harness: `dev/check` (entry
point, `dev/check`) delegating to `checklib.py`
(`dev/checklib.py`). The harness runs argv arrays
only (never shell strings), pins the child environment (locale, timezone,
`PATH`; `PYTHONPATH`/`VIRTUAL_ENV` deliberately dropped), preserves raw exit
codes, and never converts a failure into a pass.

CI (.github/workflows/ci.yml) runs the unprivileged gates only; worker-gated
suites stay `BLOCKED` there by design.

## Exit codes

| Code | Meaning |
| ---- | ------- |
| 0 | all commands succeeded |
| 1 | at least one command failed (raw child failure, propagated) |
| 2 | usage error (unknown suite, missing required flag) |
| 3 | blocked: suite needs something that is absent by design (no authorized worker, runner not implemented) — **not** a pass |
| 4 | missing prerequisite (e.g. `cargo` absent, wrong pinned `rustc` version) |

Blocked (3) is distinct from skipped: there is no silent skipping anywhere in
the harness. Missing tools are named in the reason string.

## Zero collected tests = failure

For every test-collecting suite, a run that collects **zero tests is a
failure (exit 1)**, regardless of how the underlying runner exited. An empty
test run can never be reported as a pass.

## Suite map

| Suite | Command | Contents | Worker needed? |
| ----- | ------- | -------- | -------------- |
| `unit` | `./dev/check unit` | `cargo test --workspace`; harness unit tests (`tests/harness/` test_run_contract, test_structure_lint); python dev-helper tests (`python/tests/`) | No |
| `fast` | `./dev/check fast` | `unit` + all harness tests + `dev/check_native.py` + `dev/check_seeds.py` + pack tool tests (`tests/pack/`) + contract locks (`tests/contracts/`) + `packctl validate` | No |
| `structure` | `./dev/check structure` | Static lint, no tests executed: workspace member contract, crate dependency direction, forbidden evaluation vocabulary in runtime crates, verdict protection (no `#[ignore]`, no `unittest.skip` on owned surfaces) | No |
| `contracts` | `./dev/check contracts --task T01` | Native contract checks (`tests/native/` test_contracts, test_schema), invoked per task id | No |
| `runtime` | `./dev/check runtime --task T01 --worker W` | Real runtime workloads against a registered worker | Yes |
| `differential` | `./dev/check differential --task T01 --worker W` | Candidate vs reference differential checks | Yes |
| `security` | `./dev/check security --task T01 --worker W` | Isolation/security probes | Yes |
| `parity` | `./dev/check parity --benchmark swebench --manifest M` | Benchmark parity; requires a LOCKED manifest (`swebench`, `terminalbench`, or `cybergym`) | Blocked by design* |
| `performance` | `./dev/check performance --profile P` | Performance profile run; requires a locked profile at `dev/profiles/P.json` | Blocked by design* |

\* Parity and performance are *blocked by design* today: even with a locked
manifest/profile present, the runners are not implemented and the suites
report a truthful `BLOCKED` rather than simulating a result. The same holds
for the worker-gated suites: the worker registry mechanism works, but the
adapters that would drive workers are not yet implemented.

The worker-gated suites only run against entries present in
`dev/runtime-workers.json` (ships empty); see
[setup.md#authorized-runtime-workers](setup.md#authorized-runtime-workers).

There is also an internal `selftest-exit17` suite used by the harness's own
tests to prove raw-exit-code propagation; it is deliberately not in the
public suite list.

## Run artifacts

Every suite run writes under `dev/.check-runs/`:

- `<timestamp>-<suite>.log` — raw output: argv, exit codes, child output.
- `<timestamp>-<suite>.json` — summary (suite, counts, git commit/dirty
  state, per-command metadata) with the log path and its sha256.

Keep these as evidence. Task-level evidence records follow
`specs/evidence-contract.json` and
reference sha256-hashed artifacts under `evidence/`.

## Structure lint rules (`./dev/check structure`)

- **Workspace members** must be exactly `ls-core`, `ls-api`, `ls-host`,
  `ls-guest`; no unknown crate directories.
- **Dependency direction**: `ls-core` depends on no in-repo crate;
  `ls-api`/`ls-host`/`ls-guest` may depend on `ls-core` only.
- **Forbidden vocabulary**: bare `Trial`/`Verifier`/`Reward`/`Evaluation`
  terms are lint-forbidden in runtime crates (evaluation-domain concepts do
  not belong in the runtime core).
- **Verdict protection**: no `#[ignore]` in crates, no `unittest.skip` in
  `tests/harness` or `python/tests` — skips weaken the verdict. (Known pre-existing
  debt in `tests/pack` is recorded in `plans/active/T00.md`, not silently rewritten.)

## Adding a suite

All suite logic lives in `dev/checklib.py`:

1. Implement a `suite_<name>(root[, opts])` function that returns a list of
   command records produced by the existing helpers (`cargo_test`,
   `python_unittest`, `python_script`, `run_command`) so exit codes and
   testify semantics stay uniform.
2. Register it in `_suite_defs()`.
3. Add its name to `PUBLIC_SUITES` (and to `TEST_COLLECTING` if it counts
   tests, so the zero-collected rule applies).
4. If it needs options (`--task`, `--worker`, ...), follow the existing usage
   validation pattern and return exit `2` with a reason when misused.
5. Add harness tests under `tests/harness/` for the new suite's contract
   semantics (unknown options, blocked paths, zero-test handling).
