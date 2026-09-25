# ADR 0002 — Capability-Adaptive Multi-Backend Runtime

- Status: **Proposed** (2026-09-25, Round-03); verdicts in §12 are updated by
  the round-03 UML/QEMU spike evidence before this ADR is closed.
- Scope: LightSandbox product-level runtime architecture. Supersedes the
  implicit "LightSandbox ⇒ rootless runc" reading of ADR 0001; ADR 0001 stays
  authoritative FOR THE NATIVE BACKEND ONLY.
- Evidence root: `spike/results/uml-*.json`, `evidence/runtime/uml/`
  (round-03), plus ADR 0001's `spike/results/*.json` (native).

## 1. The reframe

```text
原设计 (round-01/02):                新设计 (round-03):
LightSandbox                         LightSandbox
    ↓                                    ↓
rootless runc                    Capability Planner
                                     │
                                     ├── NativeRuncBackend   (Native)
                                     ├── UmlBackend          (Kernel)
                                     ├── QemuTcgBackend      (Machine, fallback)
                                     └── future GvisorBackend(Sentry, deferred)
```

**`runc != LightSandbox`.** runc is the *implementation of the Native backend*
and nothing more. LightSandbox is formally upgraded to a
**capability-adaptive multi-backend sandbox runtime**.

## 2. Why (the forcing facts, all measured in round-01/02)

Native rootless runc is proven viable and high-value (ADR 0001 §3: 5/5
workloads, isolation matrix 0 INTERFERABLE, warm create+exec 52–75 ms). But it
cannot natively express, on a restricted host:

| Capability gap (measured) | Consequence |
|---|---|
| multi-UID/GID (single-ID map; `chown`→EINVAL, `su`, image USER≠0) | `uids.json`: BROKEN |
| private /proc (fresh procfs EPERM; rbind host /proc ⇒ argv CONTENT_VISIBLE cross-sandbox) | `p21.json` |
| systemd/full-system semantics (no guest init, no guest cgroup tree) | structural, no pid-1 init exists |
| hard cgroup limits on the current containerized host | no writable delegation |
| privileged kernel/network semantics (iptables, guest modules) | structural |

These MUST NOT be relabelled "LightSandbox unsupported": a product-level goal
is to keep delivering a complete Agent sandbox inside ordinary Linux VMs and
even restricted outer Docker/OCI containers (user directive, round-03).

## 3. Backend vocabulary (implementation-free)

| Kind | Implementation | Target workloads |
|---|---|---|
| `Native` | runc (pinned) | high density, low start overhead; Coding/SWE/common Cyber |
| `Kernel` | Linux `ARCH=um` (UML) | full guest Linux semantics: multi-UID, private /proc, systemd, guest cgroup, iptables/kernel tasks; no KVM, no host root |
| `Machine` | QEMU TCG | machine/hardware semantics UML cannot express; final compatibility fallback |
| `Sentry` | gVisor | NOT a compatibility path now (does not solve strict-rootless multi-UID / full-system); later evaluated as a stronger-userspace-isolation backend |

Priority order: **Native → Kernel → Machine → (Sentry deferred).**

Public request vocabulary is implementation-free: `backend_preference ∈
{auto, native, kernel, machine}`. Binary names (`runc`, `linux (ARCH=um)`,
`qemu-system-*`) exist ONLY in responses/diagnostics
(`implementation` field), never as selection keys.

## 4. Selection is requirement-driven (not runtime-driven)

Users declare **what they need**, e.g.:

```yaml
requirements:
  multi_uid: true
  private_proc: true
  full_init_system: false
  hard_memory_limit: false
  network: { outbound: true, net_admin: false }
```

Flow: `requirements → HostCapabilities (observed, T41) → BackendCapabilities
(per-backend offers, measured) → BackendPlan { selected_backend,
implementation, effective_capabilities, degradations,
rejected_requirements, reason }`.

**AUTO rule v1 (deliberately simple, no scoring):** first backend in
NATIVE→KERNEL→MACHINE that is available AND observes SUPPORTED for every
hard requirement. Else reject pre-create with `HOST_CAPABILITY_UNAVAILABLE`
plus the per-backend unsatisfied-requirement list. An *explicit* preference
that cannot be satisfied rejects the same way — never a silent switch.
Unsatisfiable requirements are prerequisites, not warnings; no silent
degradation (extended from T41's resolution-rules to backend level).

Domain implementation (round-03, pure logic, test-first):
`crates/ls-core/src/domain/backend.rs` — `BackendKind`, `BackendPreference`,
`HostKind`, `CapabilityRequirements`, `BackendCapabilities`,
`BackendPlan`, `plan()`. `BackendCapabilities::native()` is a VIEW over the
T41 HostCapabilities, not a second probe.

## 5. HostKind and PrivilegeTier are separate dimensions

```text
HostKind:      VM | CONTAINERIZED | BARE_METAL        (deployment shape)
PrivilegeTier: PORTABLE_ROOTLESS | FULL_ROOTLESS | ROOTFUL  (T03 mechanism tier)
```

A VM and an outer container can BOTH be PORTABLE_ROOTLESS yet differ
measurably (outer seccomp, masked /proc, cgroup namespace). This dev worker:
**HostKind=CONTAINERIZED, PrivilegeTier=PORTABLE_ROOTLESS**. Outer-Docker
restrictions measured here MUST NOT be generalized to all VMs (and vice
versa).

## 6. task-catalog risk vocabulary (round-03 clarification)

`kind=privileged` was ambiguous ("needs host root?"). All tasks now carry:

```text
execution_risk:      READ_ONLY | LOCAL_MUTATION | RUNTIME_ISOLATION | HOST_ADMIN
requires_host_root:  true | false
```

T08 concrete resolution: `execution_risk=RUNTIME_ISOLATION`,
`requires_host_root=false` — real rootless runc creating user-owned
namespaces/mounts/runtime objects inside project-owned directories is
authorized per round-03 user grant; HOST_ADMIN tasks still require a
designated worker.

## 7. Supervisor design consequence (T43 update)

`Supervisor != runc supervisor`. The architecture block is
**InstanceSupervisor** (own backend instance lifetime, authoritative exit
state, cleanup, reconcile):

```text
NativeInstanceSupervisor  → runc / PTY / processes
KernelInstanceSupervisor  → UML process / guest control channel
MachineInstanceSupervisor → QEMU process / guest control channel
```

Common lifecycle does not depend on runc (T43 spec to be updated at T43 time).

## 8. ls-guest role upgrade

`crates/ls-guest` grows from "runc idle-init argument contract" into the
**tiny guest control agent** for kernel/machine backends: exec, signal, file,
health, shutdown over a small framed protocol on a guest channel (console).
No SSH (daemon+key+network+auth overhead); no Harbor/OpenSandbox/benchmark
logic in guest. Native-backend gains from T08: an `ls-guest` init that reaps
zombies, forwards signals and writes atomic exit receipts.

## 9. OCI remains the single canonical workload format

```text
Dockerfile → BuildKit → OCI Image
OCI Image → NativeMaterializer → host rootfs   (T06/T07)
          → UmlMaterializer    → builder-UML-produces base UBD ext disk
          → QemuMaterializer   → machine disk
```

Multi-backend does NOT fork task formats. Harbor/SWE/Cyber keep orbiting OCI.
UML materialization happens INSIDE a builder UML (guest-side unpack with real
UID/GID semantics); host exposes only a dedicated OCI cache dir, never `/`.

## 10. Harbor future architecture (design-only this round)

`Harbor Task → EnvironmentSpec → Capability Requirements → EnvironmentPlan →
ONE backend for the entire EnvironmentGroup v1`. No hybrid groups
(agent=runc, db=UML) in v1: all services Native-compatible ⇒ Native; else if
UML satisfies ⇒ whole group UML; else Machine. Simple first.

## 11. Benchmark wording and scanner

Formal parity goals stay: **100% SWE-bench / TerminalBench / CyberGym task
execution parity** — now interpreted as *LightSandbox system-level parity
across its supported software backends*, NOT "native rootless runc backend
100%". Follow-up task (post-semantics-validation): a Benchmark Capability
Scanner statically analyzing Dockerfile/Compose/USER/systemd/services/
devices/network/kernel deps → `TaskCapabilityManifest` →
Native/UML/Machine/Unknown candidate census.

## 12. Backend verdicts (filled by experiment)

| Backend | Verdict | Basis |
|---|---|---|
| Native (runc) | **ACCEPTED** | ADR 0001 evidence; T08 vertical slice (round-03) |
| Kernel (UML) | **EXPERIMENTAL** (was PENDING SPIKE) | §12.1: feasibility gate PASSED on this host — boots CONTAINERIZED+PORTABLE_ROOTLESS, no sudo/KVM, own /proc, true multi-UID + guest DAC (`spike/results/uml-boot.json` BOOT_OK). ACCEPTED still requires: OCI materialization fidelity, base+COW, exec/control transport, clean-cleanup confirmation, full perf table |
| Machine (QEMU TCG) | **CONDITIONAL** | smoke only if UML is hard-blocked by the outer container; not needed so far (UML boots) |
| Sentry (gVisor) | **DEFERRED** | doesn't solve decisive gaps; outer-seccomp sensitivity; revisit post-stability |

### 12.1 Round-03 UML spike facts (2026-09-25, host: CONTAINERIZED + PORTABLE_ROOTLESS)

- Kernel 6.19.14 (`ARCH=um`, static, initramfs, defconfig + DEVTMPFS/MAGIC_SYSRQ)
  boots as an unprivileged host process; source-locked via
  `spike/uml/source-lock.json`; binary sha256 recorded per run.
- Guest evidence (`uml-boot.json`, verdict `BOOT_OK`, markers all true):
  `LSB_PROC_OWN=yes` (guest owns /proc), `LSB_HOSTNAME=lsbx-uml`,
  `LSB_CHOWN_OK` + `LSB_STAT=1000:999 644` (chown to arbitrary uid/gid —
  impossible under the host's single-ID map), `su user -c id` →
  `uid=1000(user)`, `LSB_USER_WRITE_OK`, `LSB_DAC_ISOLATION_OK` (uid 999
  refused a uid-1000 0600 file — guest DAC enforced), `LSB_CGROUPS=6`,
  `LSB_UML_EXPERIMENT_DONE`, orderly `reboot: Power down`, host rc=0.
- Console plumbing root causes (both instrumented, both proven):
  (a) `/dev/console` **opens** but silently **drops userspace writes** on this
  host (kernel-side `console_on_rootfs` fails likewise) — evidence and any
  guest control channel MUST NOT rely on console tty writes; use `/dev/kmsg`
  (evidence) and a to-be-validated channel (serial `ssl0` or mconsole) for
  control. (b) `kernel.printk_devkmsg=ratelimit` (guest boot default) silently
  throttled userspace kmsg writes after the initial burst — evidence plumbing
  disables it (`echo on`) before emitting markers. A "missing marker" anomaly
  was bisected to this; a sleep-drain hypothesis was falsified first.
- Cleanup: orderly shutdown removes `~/.uml/<id>/`; the one observed residue
  came from an intentionally-panicked diagnostic run and was reaped manually —
  `KernelInstanceSupervisor` must reap `~/.uml/<id>` on abnormal exit.
- Performance table: NOT yet measured (boot latency, exec latency, fork-heavy,
  COW create/delete, RSS) — gating item for ACCEPTED.

## 13. UML security boundary (honest note)

A guest kernel does NOT make UML a strong-security VM: host/guest isolation
is weaker than hardware virtualization (upstream documentation is explicit).
If UML becomes a production backend, minimum hardening: no arbitrary hostfs
(restricted hostfs only for the controlled builder), module loading disabled
after boot, UML runs as an unprivileged host process, seccomp/namespaces
around the UML process where the host permits. Recorded now; implementation
is future work (T49).

## 14. Consequences for existing contracts

- contracts/v1 (T41) stays FROZEN and valid for the Native backend and the
  shared requirement-resolution semantic. The backend-selection surface
  (`backend_preference`, `BackendPlan`, new requirement dimensions
  `full_init_system`/`net_admin`) is contract EVOLUTION owned by task T45 —
  it will arrive as an additive contract change with its own freeze cycle,
  not by quietly editing the v1 files.
- ADR 0001 degradations (single-ID, proc policy, no hard limits) remain the
  NATIVE backend truth and are now exactly the reasons the planner can
  escalate to Kernel/Machine.
