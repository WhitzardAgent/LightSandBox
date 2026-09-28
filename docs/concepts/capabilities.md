# Capabilities

Capabilities are the connective tissue of LightSandbox: hosts are **measured**, requirements are **declared**, and resolution rules turn the pair into accept / degrade-and-disclose / reject. Nothing is probed once and assumed forever; every finding binds an evidence command.

## HostCapabilities: what the probe measures

Eleven read-only probes run on a host (`userns`, `uid_mappings`, `cgroup`, `procfs`, `mount`, `rootfs`, `network`, `seccomp`, `lsm`, `filesystem`, `resource`). Each returns a finding of the form `{name, status, evidence_command, exit_code, output_excerpt}` where status ∈ `SUPPORTED | DEGRADED | UNSUPPORTED | NOT_TESTED`. A finding claiming `SUPPORTED` with a non-zero exit code is structurally invalid — the validator rejects it.

From the findings the host derives a **tier**:

| Tier | Derivation | Meaning |
|---|---|---|
| `PORTABLE_ROOTLESS` | unprivileged userns creatable | First supported host class: no root, no subuid, no cgroup delegation; benchmark-task parity scope |
| `FULL_ROOTLESS` | userns **and** subuid ranges, cgroup delegation, private procfs all SUPPORTED | Rootless with full kernel cooperation |
| `ROOTFUL` | host root available | Future optional tier |
| `UNSUPPORTED_HOST` | userns not creatable | **Not an admission tier** — the honest report value when no tier can be derived; never coerced into an admission tier |

The tier gates admission; the dimensioned findings gate individual requirements. Both travel with the host report.

## Requirement dimensions: what you declare

From `contracts/v1/sandbox-requirements.schema.json` — `requires.*` are boolean **hard preconditions**:

| Requirement | Maps to host capability | Must equal |
|---|---|---|
| `multi_uid` | `identity.uid_mapping_mode` | `MULTI_ID` |
| `hard_memory_limit` | `resource.hard_memory_limit` | `SUPPORTED` |
| `private_proc` | `process.proc_policy.mode` | `PRIVATE_PROC` |
| `seccomp` | `process.seccomp` | `AVAILABLE` |
| `outbound_network` | `network.outbound` | `SUPPORTED` |

Plus two non-boolean dimensions:

- `single_id_compatibility_policy`: `STRICT` (default; rejects unfaithful UID/GID expressions — e.g. image `user: "1000"` on a single-ID host — with `POLICY_UNSUPPORTED`) or `FLATTEN_ROOT` (explicit compatibility switch; the flattening must appear in `effective_degradations`).
- `resources.*`: `memory_bytes`, `storage_bytes`, `cpu_millis`, `pids` as integer **requests/observations**. Setting a number never creates hardness — hardness comes only from `requires.*`.

## BackendCapabilities: what each backend offers

The planner holds a per-backend capability view, measured the same way as host capabilities (the Native view *is* a view over the host report, not a second probe). Planning flow:

```
requirements → HostCapabilities (observed) → BackendCapabilities (per backend)
             → BackendPlan { selected_backend, implementation,
                             effective_capabilities, effective_degradations,
                             rejected_requirements, reason }
```

Every declared dimension appears in `effective_capabilities` even when UNSUPPORTED — unsupported dimensions cannot silently disappear — and every request/effective difference appears in `effective_degradations`. An unmet `requires.*` rejects pre-create with `HOST_CAPABILITY_UNAVAILABLE` (HTTP 412); a `STRICT`-policy identity violation rejects with `POLICY_UNSUPPORTED` (HTTP 412).

Resolution cases are fixture-pinned in `contracts/v1/fixtures/resolution/cases.json` and exercised by both the Rust and Python resolvers. Examples: `multi_uid: true` on a single-ID host → reject; `resources.memory_bytes` set without `hard_memory_limit` → accepted as a request, no hardness implied; image `user: "1000"` under `FLATTEN_ROOT` → accepted with disclosed `identity.ownership_model` degradation.

## Two kinds of "hard limit"

The contract separates where enforcement lives:

| Kind | Enforced by | Status |
|---|---|---|
| **Guest-workload hard limit** | The sandbox's own kernel — only the Kernel backend has one | Memory: **proven** guest-side (guest cgroup `memory.max`, measured). cpu / pids: **unproven**, enforcement retest pending on the next kernel candidate. Guest-internal enforcement is guest-kernel truth, not a host guarantee |
| **Host-sandbox-process hard limit** | The host kernel capping sandbox processes from outside | **Unsupported** on portable-rootless hosts (no cgroup delegation); requires `FULL_ROOTLESS`/`ROOTFUL` tier hosts |

When you write `requires: {hard_memory_limit: true}`, you are asking for the capability as the planner observes it; on today's reference host class the honest resolution is *Kernel backend or rejection*, never a Native rlimit relabelled as a limit.

## Backend preference

Requests stay implementation-free: `backend_preference ∈ {auto, native, kernel, machine}`. Binary names (runc, the UML kernel, QEMU) appear only in responses' `implementation` field, never as selection keys. Details and the decision table: [backends](backends.md).
