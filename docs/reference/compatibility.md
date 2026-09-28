# Compatibility matrix

What a workload can expect, per backend and at the system level. Statuses reflect the reference host class (**containerized, portable-rootless**) as measured on the development host, 2026-09. Your host is probed independently; treat "measured" values as that host's facts, not product SLAs.

Legend: ✓ supported / proven · L limited or degraded (contract-visible) · — not available · WIP implemented path, evidence incomplete

## Level 1 — Native backend (rootless runc)

| Capability | Status | Notes |
|---|---|---|
| Common coding workloads (bash, python3, gcc, git, node) | ✓ | All five measured with correct identity, cwd, env |
| Process / mount / IPC / UTS / network namespaces | ✓ | Private per sandbox; `network=none` = loopback only |
| Filesystem isolation | ✓ | Full-copy rootfs per sandbox; write-marker tested |
| Signal / ptrace / cross-sandbox interference | ✓ | 13-axis isolation matrix, zero interference axes (measured) |
| Multi-UID (`su`, `USER` ≠ 0, unmapped `chown`) | — | Single-ID userns; rejected (`POLICY_UNSUPPORTED`) or disclosed under `FLATTEN_ROOT` |
| Private `/proc` | L | Read-only host `/proc` bind; argv + metadata visible cross-sandbox (`METADATA_AND_ARGV_VISIBLE`) |
| Hard resource limits (host-enforced) | — | No cgroup delegation on this host class |
| Seccomp filtering | L | Available on the measured host; explicit policy required, never assumed |
| Outbound network | — | No rootless network helper on the measured host; optional future work |
| Startup (warm create + exec) | ✓ | Tens of milliseconds, 52–75 ms class (measured) |

## Level 2 — Kernel backend (User-Mode Linux)

| Capability | Status | Notes |
|---|---|---|
| Guest kernel 6.19.14 (`ARCH=um`) | ✓ | Candidate built and smoke-tested; memory/pids cgroup controllers built in, modules disabled |
| Arbitrary guest UIDs, `su`, faithful file ownership | ✓ | chown to arbitrary uid/gid verified in guest |
| Private `/proc` per sandbox | ✓ | Guest-owned procfs |
| Guest DAC isolation | ✓ | Cross-uid file access refused in guest (measured) |
| Guest hard memory limit | ✓ | Guest cgroup `memory.max` enforced; resource retest 15/19 (measured) — guest-kernel truth, not a host guarantee |
| Guest hard cpu / pids limits | WIP | Enforcement unproven; next kernel candidate pending |
| Guest networking | WIP | Transport family built into the kernel; end-to-end legs not yet evidenced |
| systemd / full init | WIP | Target capability; smoke test pending |
| OCI image materialization fidelity | ✓ | 108/108 in-guest stat checks + runtime checks from on-disk config (measured) |
| Base + COW disks | ✓ | Official UBD COW; base digest unchanged across legs; ~12 MB COW floor per instance (measured) |
| Idle memory overhead | ~35 MB RSS | Per guest kernel (measured) |
| Boot time | ≤ 0.4 s micro boot; ~1.3–1.5 s with disk materialization (measured) | |
| Exec round-trip | 66.5 ms median, n=20 (measured) | |

## Level 3 — System level (via the capability planner)

| Capability | Status | Notes |
|---|---|---|
| Requirement-driven backend selection (AUTO) | ✓ contract | Native first, Kernel on requirement, else reject; fixture-pinned resolution rules |
| Explicit backend pin | ✓ contract | Unsatisfiable pin → `HOST_CAPABILITY_UNAVAILABLE`; never a silent switch |
| Prebuilt OCI images | ✓ | Digest-pinned references; materialization per backend |
| Dockerfile → image builds | Planned | Isolated BuildKit worker; not available today |
| Idle-agent launch mode | ✓ contract | Client + contract support; runtime wiring in development |
| PTY | Planned | |
| Log streaming | WIP | Framing unit-tested; runtime layer blocked |
| Harbor environments | Planned | See `tutorials/harbor.md` |
| OpenSandbox-compatible API | Planned | Adapter into native v1 |
| SWE-bench / Terminal-Bench / CyberGym parity | Not claimed | Product goal; no parity runs exist today |

## Reading this matrix

- A requirement the matrix marks L or — for a backend is not "partially supported": the planner treats it as unsatisfied for that backend and escalates or rejects. The matrix explains *why*; the `concepts/capabilities.md` define *what happens*.
- "Measured" values above are development-host numbers (2026-09). They calibrate expectations; they are not performance commitments.
