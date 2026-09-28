# Backends

LightSandbox runs workloads on one of two software backends and selects between them from your declared requirements. Both are rootless; neither needs KVM, a host kernel change, or a Docker daemon.

## Decision table

| | **Native** (rootless runc) | **Kernel** (User-Mode Linux) |
|---|---|---|
| Common coding workloads (bash, python, gcc, git, node) | ✓✓ proven | ✓ proven |
| Multiple UIDs / `su` / image `USER` ≠ 0 | Limited — single-ID namespace; `USER` ≠ 0 rejected by default policy | ✓ real guest UIDs |
| Private `/proc` per sandbox | Limited — read-only host `/proc` bind; co-located sandboxes see argv + process metadata | ✓ guest-owned `/proc` |
| systemd / full init system | No | ✓ target — smoke test pending |
| Guest hard memory limit | No (no host cgroup delegation) | ✓ proven guest-side (measured) |
| Guest hard cpu / pids limits | No | Unproven — enforcement retest pending on next kernel candidate |
| Startup overhead | Low — warm create+exec measured at tens of milliseconds | Higher — sub-second micro boot, ~1.3–1.5 s including disk materialization (measured) |
| Memory overhead per idle instance | Low (payload RSS only; no resident supervisor process in the driver itself) | ~35 MB RSS per guest kernel (measured on development host, 2026-09) |
| Isolation foundation | Shared host kernel + namespaces + single-ID userns | Own guest kernel per sandbox; weaker than hardware virtualization |

All measured values above are host-specific development-host numbers, not SLAs. The full per-requirement matrix is in the `reference/compatibility.md`.

## Automatic selection (AUTO)

You declare requirements; the planner picks the backend. Requirements come from the contract's `requires` block (`multi_uid`, `hard_memory_limit`, `private_proc`, `seccomp`, `outbound_network`) and the extended backend dimensions (`full_init_system`, `net_admin`); each maps to a measured host/backend capability per the resolution rules in `contracts/v1/`.

The AUTO rule is deliberately simple — no scoring:

1. Try **Native** first: available, and every hard requirement observed SUPPORTED? Use it.
2. Else try **Kernel** on the same test.
3. Else **reject before create** with `HOST_CAPABILITY_UNAVAILABLE` (HTTP 412) plus the per-backend list of unsatisfied requirements.

Unsatisfiable requirements are prerequisites, never warnings. Nothing is silently degraded: every difference between requested and effective capabilities is enumerated in `effective_degradations`, and unsupported dimensions must still appear in the effective capabilities — unsupported cannot silently disappear.

## Explicit override, and when to use it

You can pin a backend explicitly. The override semantics are strict: an explicit preference that the host cannot satisfy rejects with `HOST_CAPABILITY_UNAVAILABLE`, exactly like an unmet requirement — the planner **never silently switches** to a different backend.

Reasons to override:

- **Reproducibility**: a measurement or paper wants results on one specific backend.
- **Debugging**: reproduce a behavior on Native that a workload exhibits on Kernel.
- **Policy**: an operator pins untrusted or systemd-needing workloads to Kernel and bans Native for them.

If you have no such reason, stay on AUTO: it always prefers the lower-overhead backend that can honor every requirement.

## Backend status

- **Native**: create / exec / delete, background execution and cancellation implemented and integration-tested (pre-release). Degradations on portable-rootless hosts are contract-visible: single-ID, `/proc` policy `METADATA_AND_ARGV_VISIBLE`, no host-enforced hard limits.
- **Kernel**: feasibility accepted with conditions; the production backend is in development. See the `tutorials/kernel-backend.md`.
- **Machine (QEMU TCG)**: contingency only, held in reserve if UML is ever hard-blocked by an outer container.
