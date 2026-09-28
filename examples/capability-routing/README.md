# capability-routing — requirement sets to a backend plan (tested planning example)

Shows how declared requirements route to a runtime backend under the ADR 0002
capability-adaptive architecture, with every rule cited back to the frozen
contract files. This is a tested planning example: it exercises the real rule
data, not a paraphrase.

**Planning level: this example demonstrates selection logic only — the
runtime backend it plans for is still in development and ships no server
yet.** The selector demonstrated here is a small honest simulation in this
directory; the production selection is `plan()` in
`crates/ls-core/src/domain/backend.rs`, reachable only through an API server
that this repository does not ship. No sandbox is created and no backend is
started.

## Run

```bash
python3 route.py    # stdlib-only
```

## Data sources (all checked in; the script breaks loudly if any drifts)

- `contracts/v1/resolution-rules.json` — the frozen rule table: each `true`
  requirement maps to a host capability plus an accepted value; the rejection
  is `HOST_CAPABILITY_UNAVAILABLE` / HTTP 412, with no silent downgrade.
- `contracts/v1/sandbox-requirements.schema.json` — the frozen requirement
  dimensions (`multi_uid`, `hard_memory_limit`, `private_proc`, `seccomp`,
  `outbound_network`); the script asserts this set equals the
  `requirements_map` keys and refuses to run on divergence.
- `crates/ls-core/src/domain/backend.rs` — the AUTO search order
  (`BackendKind::AUTO_ORDER`), parsed from source.
- `evidence/host/hostreport-v1.json` — the measured host probe:
  user namespaces supported, but `uid_mappings` is DEGRADED (single identity
  map), so `multi_uid`'s accepted value `MULTI_ID` is not observable on the
  Native backend here.
- `spike/results/uml-boot.json` — the measured Kernel-backend (UML) spike:
  `verdict BOOT_OK` with guest multi-UID markers (`LSB_CHOWN_OK`,
  `LSB_SU_OK`, `LSB_STAT=1000:999`).

## What the two scenarios show

1. **`requires = {}`** — no hard requirements, so the first backend in the
   AUTO order (`NATIVE -> KERNEL -> MACHINE`) that is available wins:
   **plan = NATIVE** (lightest overhead; nothing required).
2. **`requires = {"multi_uid": true}`** — the cited rule demands host
   capability `identity.uid_mapping_mode == MULTI_ID`. The measured Native
   host observes `SINGLE_ID`, so the Native backend is rejected for this
   requirement (`HOST_CAPABILITY_UNAVAILABLE` / 412, never a silent
   downgrade); the AUTO walk continues and the measured Kernel offer
   satisfies it: **plan = KERNEL**.

## Verified output (captured from a real run)

```console
$ python3 route.py
=== Scenario: SandboxSpec.requires = {} ===
requested requires: {}
hard requirement dimensions: (none)
AUTO order: NATIVE -> KERNEL -> MACHINE  [crates/ls-core/src/domain/backend.rs BackendKind::AUTO_ORDER; ADR 0002 selection rule: first available backend observing SUPPORTED for every hard requirement]
plan: NATIVE (lightest available backend satisfying all 0 required dimension(s))

=== Scenario: SandboxSpec.requires = {"multi_uid": true} ===
requested requires: {"multi_uid": true}
hard requirement dimensions: ['multi_uid']
AUTO order: NATIVE -> KERNEL -> MACHINE  [crates/ls-core/src/domain/backend.rs BackendKind::AUTO_ORDER; ADR 0002 selection rule: first available backend observing SUPPORTED for every hard requirement]
rule: multi_uid -> host capability identity.uid_mapping_mode MUST be MULTI_ID  [contracts/v1/resolution-rules.json requirements_map.multi_uid]
NATIVE: observed SINGLE_ID (evidence/host/hostreport-v1.json findings.uid_mappings=DEGRADED (...))
NATIVE: requirement NOT satisfiable — rejection code HOST_CAPABILITY_UNAVAILABLE / HTTP 412; no silent downgrade, no fallback rewriting
KERNEL: observed MULTI_ID (spike/results/uml-boot.json verdict=BOOT_OK LSB_CHOWN_OK=true LSB_SU_OK=true LSB_STAT=1000:999 644)
plan: KERNEL (lightest available backend satisfying all 1 required dimension(s))

planning-level simulation only: production routing is ls-core domain::backend::plan() behind the API server (not shipped; see README.md)
```

(The `(...)` above abbreviates the measured single-ID probe excerpt, which
contains `newuidmap: command not found` and the single-identity `uid_map`.)

## Self-test

`test_example.py` runs the script and asserts both plans and their rule
citations, then tampers with a copy of the host evidence to prove the
simulation fails loudly on drift instead of falsifying a result. It runs as
part of `./dev/check examples`.
