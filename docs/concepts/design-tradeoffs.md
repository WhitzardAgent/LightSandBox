# Design trade-offs

Every structural choice in LightSandbox is a response to a measured constraint of the target environment. This page records the reasoning and what each choice costs.

## Why no KVM

The target hosts are ordinary rented Linux VMs and containerized compute — CI runners, nested containers, shared development boxes. On these hosts `/dev/kvm` is typically absent or unassignable; nested virtualization is the exception, not the rule. A runtime that requires KVM does not run where the users are. Both LightSandbox backends are therefore pure userspace: namespaces (Native) and User-Mode Linux (Kernel).

Cost: no hardware-isolation boundary anywhere in the product. The strongest isolation LightSandbox offers is a guest-kernel boundary in software (UML), which upstream documentation itself describes as weaker than hardware virtualization.

## Why rootless-first

The same hosts usually grant no root and no sudo. Rootless-first is also a blast-radius decision: if the sandbox supervisor runs as an unprivileged user, a supervisor bug starts as an unprivileged bug rather than a root bug. Rootful deployment remains possible as a future host tier, degraded-capability-free — but nothing in v1 depends on it.

Cost: some things root makes easy are unavailable rootless — cgroup delegation for hard host-side limits, pristine private procfs, subordinated UID ranges. These become explicit, contract-visible degradations rather than hidden failures.

## Why two backends

Measurement, not speculation. On the reference host class, rootless runc measurably cannot express: multiple UIDs (single-ID map → `chown` to unmapped uid fails, image `USER 1000` rejected), a private `/proc` (fresh procfs mount → EPERM), systemd/full-init semantics (no guest init exists), and hard cgroup limits (no writable delegation). Each is a real workload need. Rather than declaring them permanently unsupported, the runtime escalates to a guest-kernel backend that provides them.

## Why not runc only

Because the gaps above are structural, not engineering debt. No amount of runc configuration creates a second UID inside a single-ID user namespace or mounts a procfs the outer policy forbids. A runc-only product would have to either refuse those workloads or silently fake semantics — both unacceptable; the first shrinks the product, the second violates the no-silent-degradation rule.

## Why not UML only

UML boot time and memory cost dwarf native namespace creation, and common coding workloads need nothing UML adds.

- Native warm create+exec: measured at tens of milliseconds (52–75 ms class) with a 972 MB rootfs already copied.
- UML: sub-second micro boot, ~1.3–1.5 s with disk materialization, and ~35 MB idle RSS per guest kernel (both measured on development host, 2026-09).

At agent-benchmark density, always paying the guest-kernel tax for workloads that would run identically on Native wastes wall-clock and RAM. UML-first would also make the weaker-isolation boundary the default for *everything*, while Native's namespace isolation is the right-sized boundary for the single-trust-domain workloads that dominate the target use.

## Why not a Docker daemon

A Docker daemon is a privileged host-wide service: it requires root on the host, owns global state, and sits on every candidate execution path — precisely what the rootless, per-user, per-sandbox-supervisor model avoids. LightSandbox reuses two pieces of the OCI ecosystem deliberately: **runc** (as the Native driver, pinned and invoked directly, unprivileged) and an **isolated BuildKit worker** (for image builds, planned) — never the daemon.

## What we lose, honestly

- **No swappable kernels on Native.** The guest shares the host kernel; kernel-version-sensitive workloads must run on the Kernel backend.
- **UML isolation is weaker than hardware virtualization.** A guest-kernel exploit chain has a softer boundary to cross than a VM escape would. Hardening (no arbitrary hostfs, modules disabled, unprivileged host process, seccomp where the host permits) is roadmap, not shipped.
- **Kernel backend overhead.** ~35 MB RSS and ~1 s-class boot per instance bounds density (measured on development host, 2026-09).
- **No host-enforced hard limits on portable-rootless hosts.** Without cgroup delegation, only guest-internal enforcement (Kernel) or process-level rlimit guardrails (Native) exist; neither is a host guarantee.
- **Platform scope.** Linux x86_64 only, for now. No macOS or Windows host support; the development checks run there only partially.
