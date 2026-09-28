# Tutorial: the Native backend

The Native backend runs sandboxes as rootless runc containers: your payload gets private PID/mount/IPC/UTS/network namespaces inside a single-ID user namespace, on a full-copy root filesystem. This tutorial explains what your host must provide, how LightSandbox finds out, and which degradations are declared on the reference host class.

## The gate: host probe → tier → admission

Before any sandbox is admitted, an eleven-probe host report answers "what can this host actually do?" Each probe is read-only, binds an evidence command, and rolls up into a **tier**:

```
11 probes  →  findings (status + evidence)  →  derived tier  →  admission gate
```

| Tier | Gate for Native |
|---|---|
| `PORTABLE_ROOTLESS` | Admitted — with the degradations below contract-visible |
| `FULL_ROOTLESS` | Admitted — degradations re-measured per tier (several lift) |
| `ROOTFUL` | Future optional tier |
| `UNSUPPORTED_HOST` | Refused. This value is never coerced into an admission tier |

The probes and tier rule live in `python/src/lightsandbox_dev/host_probe.py` (Python reference) and mirror `ls-core`'s Rust admission logic; the frozen per-dimension schema is `contracts/v1/host-capabilities.schema.json`.

## The portable-rootless profile

The reference host class — a nested container on an unprivileged VM — measures as `PORTABLE_ROOTLESS`. Its profile, as checked into the repo's host records:

- userns **SUPPORTED**: unprivileged user namespaces can be created — this alone derives the tier.
- uid_mappings / cgroup / procfs / resource **degraded or unsupported**: no subuid ranges, no writable cgroup delegation, fresh private procfs mount returns EPERM, no user-slice delegation (rlimits are process-level guardrails, never sold as hard limits).
- mount / rootfs / network / seccomp **mixed**: tmpfs + bind mounts work inside userns+mountns; overlay disk uppers and `/dev/fuse` are unavailable, so rootfs = full copy (memory COW layer proven); private netns works but no rootless network helper exists (`network=none` = loopback only); seccomp available via a purpose-built runc.

What this profile *forbids claiming* — recorded alongside the profile: full multi-UID semantics, host-enforced cgroup hard limits, and a private procfs per sandbox on this layer.

## What the degradations mean in practice

| You ask for | Native on portable-rootless answers |
|---|---|
| Default sandbox (no requirements) | Accept — degradations about `/proc` visibility still enumerated in effective capabilities |
| `requires.multi_uid: true` | Reject `HOST_CAPABILITY_UNAVAILABLE` — planner escalates to Kernel if allowed |
| Image with `user: "1000"` | Reject `POLICY_UNSUPPORTED` under the default `STRICT` policy; accept with disclosed flattening under `FLATTEN_ROOT` |
| `requires.private_proc: true` | Reject — the measured mode is `RBIND_RO_HOST_PROC`, class `METADATA_AND_ARGV_VISIBLE` |
| `requires.seccomp: true` | Accept on hosts whose runc build carries seccomp (the measured dev host does); reject on hosts without it — never silently unfiltered |
| `requires.hard_memory_limit: true` | Reject — no cgroup delegation; a memory number in `resources.*` is a request, not a limit |

These outcomes are not prose: they are pinned in `contracts/v1/fixtures/resolution/cases.json` and re-verified by the resolver test suites.

## Try it

The probe tooling ships in the repo as `python/src/lightsandbox_dev/host_probe.py` (run it as a script on a Linux host; it writes `evidence/host/hostreport-v1.json`).

!!! note "Runs real probes"
    The probe executes read-only argv probes (`unshare`, namespace-local mounts in throwaway tmp dirs) and rewrites the checked-in development host report. Run it on a host you own; it is a measurement tool, and its exit code is non-zero on hosts outside the admission tiers. It is Linux-only — the probes use Linux namespace syscalls.

## Performance character (measured on development host, 2026-09)

- Rootfs prepare (972 MB full copy): ~0.8–0.9 s; delete ~0.2 s.
- Warm create + exec (`/bin/true`): 52–75 ms class; python3 payload: 70–155 ms class.
- Idle payload RSS in the hundreds of KB; no resident supervisor process in the driver itself.

Next: [the Kernel backend](kernel-backend.md) for the full-Linux path, or [backend selection](backend-selection.md) for the planner's point of view.
