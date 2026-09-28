# Tutorial: the Kernel backend

The Kernel backend gives each sandbox its own Linux kernel — User-Mode Linux (UML) running as an unprivileged host process. It exists for the workloads the Native backend structurally cannot serve: multiple UIDs, a private `/proc`, guest cgroup limits, systemd, kernel-level networking.

## Status: feasibility accepted, production backend in development

UML feasibility is **accepted with conditions**. The evidence chain is real; the conditions are open, named, and tracked — not hidden debt. There is no production kernel backend you can run today.

## The current kernel candidate

Measured facts about the current candidate (built and smoke-tested on the development host, 2026-09):

- Linux **6.19.14**, `ARCH=um`, source-pinned with digests recorded.
- `CONFIG_MEMCG` and `CONFIG_CGROUP_PIDS` **built in**; kernel modules **disabled** entirely (policy: anything needed is built-in — a `=m` symbol in a distribution-less initramfs is dead weight that pretends to be available).
- Dynamically linked.
- Boots as an ordinary unprivileged host process — no root, no KVM, no host kernel change.

Earlier candidates taught two plumbing lessons now baked into the design: the guest console can silently drop userspace writes on nested hosts (so control and evidence use a serial framed channel and `/dev/kmsg`, never the console), and a guest `printk_devkmsg` ratelimit can throttle marker output (disabled before evidence runs).

## What works today (measured in the guest)

| Capability | Evidence |
|---|---|
| Guest owns its `/proc`, hostname, UID space | Boot markers: `chown` to arbitrary uid:gid works, `su user` → `uid=1000`, uid-999 process refused a uid-1000 `0600` file (guest DAC enforced) |
| OCI image fidelity | 108/108 in-guest stat checks plus cwd/argv/uid/env runtime checks, all from the on-disk OCI config; the builder exposes only a dedicated read-only OCI cache dir to the guest |
| Base image + COW instances | Official UBD copy-on-write; base digest unchanged after every leg; ~12 MB COW allocation floor per instance, 12 ms delete |
| Exec/control transport | Framed serial channel; exit code / cwd / argv faithful; binary-safe at 64 KB sha256-exact |
| Cleanup | Orderly poweroff self-reaps guest state; a killed guest leaves residue exactly once and a single supervisor sweep removes it |
| Guest hard **memory** limit | Guest cgroup `memory.max` enforced under a resource-semantics retest (15/19 checks passing) — guest-kernel truth, not a host guarantee |

Performance on the development host, 2026-09: micro boot ≤ 0.4 s (control initramfs), ~1.3–1.5 s including disk materialization; exec round-trip 66.5 ms median (n=20); idle RSS ~35 MB per guest kernel independent of the `mem=` parameter; 8 × 64 MB guests coexisted in 276 MB total with a 211.9 ms exec round-trip under contention.

## What is still WIP

- **Guest cpu / pids hard limits** — enforcement unproven; the next kernel candidate and retest are pending.
- **Guest networking** — the transport family is now built into the kernel, but no end-to-end leg (slirp, DNS, inbound) is evidenced yet.
- **systemd / full init** — target capability, smoke test pending.
- **Persistent guest control agent** — cancellation semantics over a long-lived guest agent are not yet exercised (the current handler is synchronous).
- **Hardening exercise** — no arbitrary hostfs, unprivileged containment of the UML process, seccomp where the host permits: recorded, not implemented.

These are the standing conditions of "accepted with conditions" and the reason no production claim exists.

## Isolation honesty

A guest kernel per sandbox buys real guest-side semantics (UIDs, `/proc`, cgroups, DAC) — and buys them without KVM. It is a softer host/guest boundary than hardware virtualization; the `concepts/security-model.md` gives the full picture, and the `concepts/design-tradeoffs.md` explain why Native remains the default for workloads that need none of this.

Next: [backend selection](backend-selection.md) — how a requirement like `multi_uid` routes a workload here automatically.
