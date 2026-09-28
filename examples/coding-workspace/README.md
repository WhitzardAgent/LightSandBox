# coding-workspace — workspace fixture staged as a workload plan (planning level)

A coding-task-shaped workload: a tiny git-less source tree with a failing
test, staged the way the native API would receive it — an offline environment
plan plus a dry-run exec request for the verifier command. Adapted from the
round-05 "coding workspace" goal: the original goal (edit code in a sandbox
and run the tests there) requires a live runtime, so this example stops at a
validated plan and honestly says so.

**Planning level: runtime execution requires a runtime worker — not yet
available.** Nothing in this example runs the workload; no test is executed
inside a sandbox and no result is fabricated. Every printed request carries
`executed: false` from the real reference CLI.

## Layout

- `workspace/calc.py` — module under test; `square()` is deliberately wrong.
- `workspace/test_calc.py` — the verifier test; it **fails** against the
  fixture as designed (`assertEqual(square(3), 9)` vs `x + x == 6`). A real
  coding agent task would be "make this pass".
- `environment.json` — the workload service: digest-pinned example image,
  guest cwd `/workspace`, env, and resource observations in canonical units.
- `stage_workload.py` — the staging script described below.

## Run

```bash
python3 stage_workload.py
```

Requires Python >= 3.11 and the repo's reference dependency `httpx`. Runs
fully offline.

## What the staging shows

1. **The fixture** — listed, never executed by the example itself.
2. **The environment plan** — `tools/lsbx.py env plan` validates
   `environment.json` and emits a deterministic `plan_digest` with
   `execution_ready: false`; the unresolved list names exactly what is still
   missing (host capabilities, image materialization, template preparation).
3. **The exec preview** — the exact HTTP request body a pytest verifier run
   would use: `argv: ["python", "-m", "pytest", "-q"]`, `cwd: /workspace`,
   idempotency key, generation — with `executed: false`.

## What is deliberately missing (would add, once a runtime worker exists)

- Staging the host `workspace/` tree into the guest. Mounts/volume staging
  are not part of the offline planning subset — the plan only says "the image
  + cwd + command", and a runtime would have to place the tree at
  `/workspace`.
- Materializing the pinned image into a guest rootfs.
- Actually running pytest inside the sandbox and returning a faithful exit
  status. Each of those sentences describes behavior that does not exist yet
  in this repository.

## Self-test

`test_example.py` runs the staging script, asserts the planning-only markers
and the exact request shape, and separately proves the fixture's failing test
genuinely fails (so nobody can mistake the fixture for a passing verifier).
It runs as part of `./dev/check examples`.
