# LightSandbox

[![dev-gates](https://github.com/WhitzardAgent/LightSandBox/actions/workflows/ci.yml/badge.svg)](https://github.com/WhitzardAgent/LightSandBox/actions/workflows/ci.yml)

LightSandbox is a lightweight, rootless-first sandbox runtime for coding and agent workloads. It runs inside ordinary Linux VMs and containerized compute without KVM, and automatically selects between a fast native backend and a full-Linux compatibility backend.

> **Status: pre-release.** This repository ships frozen wire contracts, a tested native API client, and tested backend components — **no runtime server ships yet**. See [Current status](#current-status).

## Core value

- **Rootless-first**: built for hosts with no root, no sudo, and no KVM — the common shape of rented Linux VMs and nested CI containers.
- **Requirement-driven**: you declare what a workload needs (multiple UIDs, a private `/proc`, systemd, a hard memory limit). A capability planner matches requirements against measured host capabilities and picks a backend — or rejects the request with an explicit error instead of silently degrading it.
- **Contract-first**: the CLI, Python SDK, and HTTP API share one frozen contract (`contracts/v1/`). A request the host cannot honor fails before creation with `HOST_CAPABILITY_UNAVAILABLE`; every accepted degradation is enumerated in the response.
- **Measured, not assumed**: each host capability is derived from live probes and recorded with evidence. Numbers in this documentation come from those measurements and say so.

## Quick start — development baseline (no runtime server ships yet)

Everything below runs offline from the repository root: no daemon, no container runtime, no root. See [installation](docs/getting-started/installation.md) for prerequisites.

```bash
# Validate the planning pack (structure and evidence references)
python3 tools/packctl.py validate

# Run the native contract reference tests
python3 dev/check_native.py

# Reference client, local mode (no network)
python3 tools/lsbx.py version

# Offline environment planning: validates a spec, emits a plan digest
python3 tools/lsbx.py --json env plan --file examples/native/environment.json

# Preview the exact HTTP request a create would send
python3 tools/lsbx.py --json sandbox create --dry-run --image example.invalid/app:latest

# Preview an exec request
python3 tools/lsbx.py --json exec sb_demo --generation 1 --dry-run -- echo hello
```

Without `--dry-run`, client commands issue real HTTP requests to a compatible daemon over `unix:///run/lightsandbox/api.sock` and fail today, because this package provides no server — read requests (GET) surface `TRANSPORT_ERROR: cannot reach sandbox API`, while mutating requests (POST/DELETE) surface `REQUEST_OUTCOME_UNKNOWN`, since a lost response leaves the request's outcome uncertain. The [quickstart](docs/getting-started/quickstart.md) walks through each command with expected output.

## Backends

| Backend | Driver | Status today | Characteristics |
|---|---|---|---|
| **Native** | rootless runc | Implemented (pre-release): create / exec / delete, background exec, cancellation — unit and integration tested | Low startup overhead. On portable-rootless hosts: single-ID user namespace, host `/proc` shape read-only visible, no host-enforced hard resource limits (cgroupfs is read-only there) |
| **Kernel** | User-Mode Linux (UML) | Feasibility accepted with conditions; production backend in development | Real guest kernel: arbitrary UIDs, private `/proc`, guest cgroup memory hard limit proven guest-side; cpu/pids limit enforcement unproven; guest networking and systemd pending |
| **Machine** | QEMU TCG | Contingency only | Evaluated only if UML is hard-blocked by the outer environment; not needed so far |
| **Sentry** | gVisor | Deferred | Revisited after the two primary backends stabilize |

The planner tries Native first (lower overhead) and escalates to Kernel only when a hard requirement demands it. An explicit backend request that cannot be satisfied is rejected with `HOST_CAPABILITY_UNAVAILABLE` — never silently switched. Details: [Backends](docs/concepts/backends.md), [Compatibility matrix](docs/reference/compatibility.md).

## Current status

| Capability | Status |
|---|---|
| Native create / exec / delete | ✓ Implemented (unit + integration tests, pre-release) |
| Background execution + cancellation | ✓ Implemented |
| Log streaming | WIP (unit tests complete, runtime layer blocked) |
| Kernel/UML feasibility | ✓ Accepted with conditions |
| Kernel production backend | WIP |
| Guest workloads: memory hard limit | ✓ Proven in guest (guest cgroup, measured) |
| Guest workloads: cpu / pids hard limits | Unproven (kernel candidate in progress) |
| Host-enforced sandbox hard limits | Unsupported on portable-rootless hosts (no cgroup delegation) |
| PTY | WIP (unit core + design freeze; runtime legs worker-gated) |
| Dockerfile builds | Planned (prebuilt OCI images only today) |
| Harbor provider | Planned |
| OpenSandbox-compatible API | Planned |
| SWE-bench / Terminal-Bench / CyberGym parity | Not yet claimed |

## Security model, in brief

LightSandbox reports what it isolates and what it does not — per host, with evidence. The honest summary:

- The **Native backend shares the host kernel** with every sandbox on that host.
- On portable-rootless hosts, sandboxes run in a **single-ID user namespace**; a co-located sandbox can read the **argv and process metadata** of other sandboxes via the read-only host `/proc` bind (environment memory pages are denied on the measured host, but that deny is a host-profile fact, not a portable guarantee). Portable-rootless workers are single-trust-domain workers.
- The **Kernel backend** gives each sandbox its own guest kernel, `/proc`, and UID space, with weaker host/guest isolation than hardware virtualization.
- Never pass secrets on a sandbox command line (argv is public to co-located sandboxes on the native backend).

Full checklist: [Security model](docs/concepts/security-model.md).

## Architecture

```
        ┌──────────────────────────────────────┐
        │  Native API v1 (CLI · Python SDK · HTTP) │
        └──────────────────┬───────────────────┘
                           │  requirements + host capabilities
                    Capability Planner
                     (no silent downgrade)
                           │
            ┌──────────────┴──────────────┐
            ▼                             ▼
    Native backend                  Kernel backend
    rootless runc                   UML guest kernel
    per-sandbox supervisor          serial control channel
```

One sandbox = one lightweight supervisor instance owning lifecycle, exit state, and cleanup. Closing an API connection never destroys guest resources; after a lost execution result there is no implicit replay. Deep dive: [Architecture](docs/concepts/architecture.md).

## Planned integrations

Both are in development; neither is available today.

- **Harbor provider** — maps Harbor's task/environment model onto LightSandbox environment services ([design notes](docs/tutorials/harbor.md)).
- **OpenSandbox-compatible API** — adapter into the same native v1 contract.

## Documentation

- [Getting started](docs/getting-started/installation.md) · [Quickstart](docs/getting-started/quickstart.md) · [First sandbox (runtime-gated)](docs/getting-started/first-sandbox.md)
- Concepts: [Architecture](docs/concepts/architecture.md) · [Backends](docs/concepts/backends.md) · [Capabilities](docs/concepts/capabilities.md) · [Security model](docs/concepts/security-model.md) · [Design trade-offs](docs/concepts/design-tradeoffs.md)
- Tutorials: [Run a sandbox (offline flow)](docs/tutorials/run-a-sandbox.md) · [Native backend](docs/tutorials/native-backend.md) · [Kernel backend](docs/tutorials/kernel-backend.md) · [Custom images](docs/tutorials/custom-image.md) · [Backend selection](docs/tutorials/backend-selection.md) · [Harbor (planned)](docs/tutorials/harbor.md)
- Reference: [Compatibility matrix](docs/reference/compatibility.md)
- [Contributing](CONTRIBUTING.md) · [Security policy](SECURITY.md)
- [ROADMAP.md](ROADMAP.md) (internal roadmap) · [ADR 0001](docs/adr/0001-portable-rootless-runtime.md) and [ADR 0002](docs/adr/0002-capability-adaptive-backends.md) (internal design docs) · [Full doc index](docs/index.md)

## Repository layout

| Path | Contents |
|---|---|
| `contracts/v1/` | Frozen native v1 contract: schemas, resolution rules, state machines, fixtures |
| `crates/` | Rust workspace: `ls-core` (domain), `ls-api` (wire vocabulary), `ls-host` (host runtime), `ls-guest` (guest PID 1 helper) |
| `client/python/` | Reference client: package `lightsandbox-client-reference`, console script `lsbx` |
| `tools/`, `dev/` | Repo checks and development gates (`packctl.py`, `lsbx.py` shim, check scripts) |
| `docs/v5/`, `docs/adr/`, `progress/`, `plans/` | Internal design and process documents |
