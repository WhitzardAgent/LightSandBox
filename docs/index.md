# LightSandbox documentation

LightSandbox is a lightweight, rootless-first sandbox runtime for coding and agent workloads. It runs inside ordinary Linux VMs and containerized compute without KVM, and selects between a fast native backend (rootless runc) and a full-Linux compatibility backend (User-Mode Linux) based on declared requirements.

**Pre-release status:** frozen contracts, a tested reference client, and tested backend components ship today; the runtime server does not. Every page in this documentation marks runtime-gated material explicitly.

## Pick your path

### I want to use sandboxes — *User*

1. [Installation](getting-started/installation.md) — prerequisites and the development baseline
2. [Quickstart](getting-started/quickstart.md) — clone to first (offline) plan in five commands
3. [Your first sandbox](getting-started/first-sandbox.md) — the create → exec → exit → delete flow (runtime-gated)
4. [Backends](concepts/backends.md) — which backend serves your workload
5. [Compatibility matrix](reference/compatibility.md) — per-feature truth table

### I want to embed sandboxes in a product or harness — *Integrator*

1. [Architecture](concepts/architecture.md) — contract, planner, supervisor, crate map
2. [Capabilities](concepts/capabilities.md) — requirement dimensions and resolution rules
3. [Backend selection](tutorials/backend-selection.md) — accept / degrade / reject, fixture by fixture
4. [Custom images](tutorials/custom-image.md) — the OCI-only workload format
5. [Harbor integration](tutorials/harbor.md) — planned provider, design goals
6. [Security model](concepts/security-model.md) — trust domains and secret-handling rules

### I want to work on LightSandbox — *Contributor*

1. [Installation](getting-started/installation.md) — pinned toolchain (Rust 1.98.1, Python ≥ 3.11)
2. [Architecture](concepts/architecture.md) — where logic lives (`ls-core` domain, `ls-api` wire, `ls-host` runtime, `ls-guest`)
3. [Design trade-offs](concepts/design-tradeoffs.md) — why the system is shaped this way
4. Tutorials on the two backends: [Native](tutorials/native-backend.md), [Kernel](tutorials/kernel-backend.md)
5. Then the repo-internal entry points: `START_HERE.md`, `AGENTS.md`, and `CONTRIBUTING.md` at the repository root

### I want to operate or release it — *Maintainer*

1. [Security model](concepts/security-model.md) — what may and may not be claimed
2. [Compatibility matrix](reference/compatibility.md) — capability truth per backend
3. Repo-internal process documents (not part of this published site): `ROADMAP.md`, `docs/adr/` (architecture decision records), `progress/` and `plans/` — all marked internal in the repository

## Conventions in these docs

- Commands shown in *Getting started* and *Tutorials* run offline from the repository root today, unless a page is explicitly badged runtime-gated or planned.
- Measured performance numbers always carry their context ("measured on development host, 2026-09") and never stand in for SLAs.
- Internal design documents (`docs/v5/`, `docs/adr/`, `progress/`, `plans/`) are deliberately excluded from this site; where they are mentioned, they are labeled internal.
