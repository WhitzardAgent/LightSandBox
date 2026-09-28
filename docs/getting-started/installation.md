# Installation

LightSandbox is pre-release software. What you install today is the **development baseline**: the reference client, the frozen contracts, the Rust crates, and the repo's check tooling. **No runtime server ships yet** — the client talks to an external compatible daemon that this package does not provide.

## Prerequisites

| Requirement | Version | Needed for |
|---|---|---|
| Linux, x86_64 | any recent distribution | everything (the runtime targets Linux hosts; macOS/Windows can browse the repo but cannot run the backends) |
| Python | ≥ 3.11 | reference client, repo check tooling |
| `httpx` + `jsonschema` | see `dev/requirements-reference.txt` | reference client and contract test dependencies (currently `httpx==0.28.1`, `jsonschema==4.26.0`) |
| Rust | **1.98.1** (pinned in `dev/toolchain.json`) | building and testing the crates in `crates/` |

Not needed on the development baseline:

- **No Docker or containerd daemon.** LightSandbox never uses a Docker daemon on its execution path; the checks below are container-free.
- **No root or sudo.** The checks and the reference client run as an ordinary user.
- **No KVM.** Present on no supported host class by design.

The runtime itself (when it ships) will require a pinned runc for the Native backend; that dependency is managed by the repo's source locks, not by your system package manager.

## Clone

```bash
git clone https://github.com/WhitzardAgent/LightSandBox.git
cd LightSandBox
```

## Install the Python reference dependencies

```bash
python3 -m pip install -r dev/requirements-reference.txt
```

`dev/requirements-reference.txt` records versions verified in this authoring environment. The reference client itself is used **in place** via the repo shim `tools/lsbx.py` — there is no published package to install yet.

## Verify the checkout

Run from the repository root:

```bash
# Planning-pack structure and evidence references
python3 tools/packctl.py validate

# Native contract reference checks (client, contract fixtures)
python3 dev/check_native.py

# Seed package reference checks (older internal protocol fixtures)
python3 dev/check_seeds.py

# Pack-level test suite
python3 -m unittest discover -s tests/pack -v
```

All four must pass on a clean checkout. Together they validate structure, contracts, and reference logic — they do **not** certify a running sandbox; no runtime exists yet to certify.

## Rust toolchain (for crate work)

```bash
rustup toolchain install 1.98.1 --profile minimal
rustup default 1.98.1
cargo test --workspace
```

The repo gates verify the pinned toolchain before running crate tests (`dev/check` enforces `dev/toolchain.json`).

## What you can run today

| Command | What it does |
|---|---|
| `python3 tools/lsbx.py version` | Prints client/contract versions locally; no network |
| `python3 tools/lsbx.py --json env plan --file examples/native/environment.json` | Offline environment planning: validates a spec and emits a `plan_digest` with `execution_ready: false` |
| `python3 tools/lsbx.py --json sandbox create --dry-run ...` | Prints the exact HTTP request shape with `executed: false` |
| any non-`--dry-run` client command | Performs real HTTP to a compatible daemon — fails today because no server ships in this repo: reads with `TRANSPORT_ERROR`, mutations with `REQUEST_OUTCOME_UNKNOWN` (see [error codes](../reference/errors.md)) |

Next: the [quickstart](quickstart.md) walks each command with expected output, and [first sandbox](first-sandbox.md) explains the end-to-end flow the runtime will provide.
