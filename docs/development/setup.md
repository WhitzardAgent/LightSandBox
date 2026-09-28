# Development Environment Setup

This guide gets you from a clean checkout to running the local gates. Nothing
here requires Docker, root, or a Linux machine: the unit and reference gates
are unprivileged. Runtime testing happens only on separately authorized
workers (see [Authorized runtime workers](#authorized-runtime-workers)).

## Prerequisites

- `git`
- **Python ≥ 3.11** (the check harness uses `tomllib` and modern `unittest`).
- **rustup**, used to install the pinned Rust toolchain.
- Network access only for initial tool/download steps; the check suites
  themselves run offline.

## Rust toolchain

The Rust toolchain is pinned to **1.98.1** in
`dev/toolchain.json` and the pin is *verified by
`dev/check` before `cargo test`* — a wrong toolchain version makes suites exit
with code `4` (missing prerequisite), so install the pin exactly:

```bash
rustup toolchain install 1.98.1 --profile minimal
rustup default 1.98.1
rustc --version   # must print 1.98.1
```

`dev/check` prepends `~/.cargo/bin` to `PATH` when `cargo` is not otherwise
available, but installing rustup's standard `~/.cargo/bin` `PATH` entry
yourself is the robust option.

## Python environment

Use a virtual environment so the pinned reference packages do not fight your
system Python:

```bash
python3.11 -m venv .venv          # or python3.12 / python3
source .venv/bin/activate
python -m pip install httpx==0.28.1 jsonschema==4.26.0
```

The exact pins come from
`dev/requirements-reference.txt`: that
file records the versions actually present in the authoring environment and
is the reference for development installs (it is *not* a production
dependency lock). Per `AGENTS.md`, install dependencies only
in an environment you are authorized to modify. You do not need the venv to
run `dev/check`: the harness drops `PYTHONPATH`/`VIRTUAL_ENV` inheritance and
inserts its own `sys.path` entries deliberately, but the two packages above
must be importable by the interpreter that runs the reference checks.

A note on interpreters: `dev/check` requires Python ≥ 3.11 and will re-exec
itself with `python3.12`/`python3.11` if you launch it under an older
`python3`. The standalone reference scripts (`dev/check_native.py`,
`dev/check_seeds.py`) do **not** re-exec — run them with a ≥ 3.11 interpreter
explicitly.

## Verify the checkout

```bash
python3 tools/packctl.py validate     # pack integrity: catalogs, links, states
./dev/check fast                      # the daily gate: unit + harness + reference + pack
```

`packctl validate` traverses every internal Markdown link in the repository,
so keep links in any docs you add pointing at real files.

## Test suites

See [testing.md](testing.md) for the full suite map, exit-code semantics, and
how to add a suite. The short version:

```bash
./dev/check unit         # cargo test --workspace + python unit layers
./dev/check fast         # everything unprivileged
./dev/check structure    # static lint: dep direction, forbidden vocabulary, verdict protection
./dev/check contracts --task T01   # contract checks for one task
python3 dev/check_native.py && python3 dev/check_seeds.py
python3 -m unittest discover -s tests/pack -v
```

Every run drops its raw log and a sha256-referencing JSON summary under
`dev/.check-runs/` — keep those as evidence when you report results.

## Authorized runtime workers

Some suites (runtime, differential, security) mutate hosts and run real
isolation workloads. They execute **only** against hosts explicitly
registered in `dev/runtime-workers.json`, which ships empty so those suites
report exit code `3` (`BLOCKED`) by default. That is an honest result, not a
failure to fix.

To register a worker you control and are authorized to use:

1. Copy the entry shape from
   `dev/runtime-workers.example.json`
   into `dev/runtime-workers.json` (this file is local state; the template
   documents each field: `authorized`, `host`, host capability `tier`,
   `registered_by`, `registered_on`, `notes`).
2. Record the host capability tier (`PORTABLE_ROOTLESS` / `FULL_ROOTLESS` /
   `ROOTFUL`) from an actual probe, not an assumption.
3. Invoke the suite with the worker id:
   `./dev/check runtime --task T01 --worker WORKER_ID`.

Note that as of this writing the runtime/differential/security **adapters are
not implemented** in the harness, so even a registered worker yields a
truthful `BLOCKED` — the registry mechanism and its failure semantics are
what currently exist and are tested.

## Verifying the pack manifest

`python3 tools/packctl.py verify-manifest` checks the checkout byte-for-byte
against the shipped `MANIFEST.json` (file set, sizes, sha256). Use it to
confirm a pristine archive. **Normal development is expected to break this
manifest** — do not regenerate or rewrite it to make a modified tree look
pristine; the manifest is historical evidence of the original pack.

## What setup deliberately does not include

- No Docker / containerd daemon on the development path.
- No host mutation, privileged testing, or kernel changes from the unprivileged
  gates.
- No download of upstream sources during checks; upstream source locks are a
  separate, recorded step owned by specific tasks.
