# Examples

Runnable reference examples. Everything here runs offline against the
reference client and the frozen contracts — **no sandbox server ships in this
repository**, so every request preview carries `executed: false` from the real
CLI. Status "planning" means: real client/contract logic, stopping before any
daemon call. Five runnable examples, two pre-existing fixture trees.

## Runnable examples

| Example | Status | What it shows |
|---|---|---|
| [hello/](hello/) | runnable, planning level | client version, one validated `ExecSpec`, dry-run create + exec for `echo hello` |
| [python/](python/) | runnable, planning level | typed client surface (`SandboxRef`/`ExecSpec`/`ExitStatus`), `resolve_launch`, `lsbx env plan` |
| [coding-workspace/](coding-workspace/) | runnable, planning level | git-less workspace fixture with a failing test, staged as env plan + pytest dry-run; runtime execution requires a runtime worker — not yet available |
| [custom-image/](custom-image/) | runnable, planning level | checked-in minimal OCI image layout, digest-chain verification, pin-by-digest guidance (Dockerfile build: planned, not available) |
| [capability-routing/](capability-routing/) | runnable, tested planning example | requirement sets -> backend plan with rule citations from `contracts/v1` and measured evidence; planning level until the runtime backend work lands |

Each runnable example carries `test_example.py`; run them all with
`./dev/check examples` (they also run standalone, e.g.
`python3 examples/hello/main.py`).

## Pre-existing fixtures (unchanged)

| Directory | Status | Contents |
|---|---|---|
| [native/](native/) | reference fixtures | offline native v1 env/exec JSON for `tools/lsbx.py env plan` and `--dry-run`; image references deliberately unpullable |
| [harbor/](harbor/) | config-only fixtures | Harbor task layouts (`minimal`, `multiservice`, `separate-verifier`): single-env file oracle, HTTP sidecar + shared volume, separate verifier image via top-level artifacts |

The harbor package performs configuration/directory checks only — nothing is
built or executed here, and these are not substitutes for the three formal
benchmarks. Image tags are not yet digest-pinned; run gates require locking
upstream sources and images first.
