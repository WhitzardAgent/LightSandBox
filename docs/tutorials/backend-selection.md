# Tutorial: backend selection

How the capability planner turns a sandbox spec into a backend decision — accept, accept-with-disclosed-degradation, or reject — using the real fixtures that pin this behavior. Everything here is conceptual contract material: the examples cite `contracts/v1/`, and nothing on this page requires (or performs) a live run.

## The two hosts in this story

The resolution fixtures ship two measured hosts (see `contracts/v1/fixtures/resolution/cases.json`):

- **Host A — portable-rootless**: single-ID user namespace; `/proc` is a read-only host bind with class `METADATA_AND_ARGV_VISIBLE`; no cgroup delegation (all hard limits UNSUPPORTED); seccomp AVAILABLE; no outbound network.
- **Host B — full-rootless**: multi-ID mappings, faithful ownership, private `/proc`, delegated cgroups (all hard limits SUPPORTED), overlay rootfs, outbound network.

Same API, same spec — different truth underneath.

## The requirement dimensions

A spec declares hard preconditions in `requires` plus secondary dimensions (full list and capability mapping: `concepts/capabilities.md`):

| `requires.*` | Needs on the host | Host A | Host B |
|---|---|---|---|
| `multi_uid` | `identity.uid_mapping_mode = MULTI_ID` | ✗ | ✓ |
| `hard_memory_limit` | `resource.hard_memory_limit = SUPPORTED` | ✗ | ✓ |
| `private_proc` | `process.proc_policy.mode = PRIVATE_PROC` | ✗ | ✓ |
| `seccomp` | `process.seccomp = AVAILABLE` | ✓ | ✓ |
| `outbound_network` | `network.outbound = SUPPORTED` | ✗ | ✓ |

Resource *numbers* (`resources.memory_bytes`, `cpu_millis`, `pids`, `storage_bytes`) are requests and observations — setting them never conjures a hard limit.

## Walk the fixtures

**Baseline, no requirements (fixture A1).** Any image, no `requires`. Host A: **accept**. No rejection, no degradation to disclose — but the effective capabilities still enumerate *every* dimension, including the unsupported ones. Unsupported never silently disappears; the response is a complete capability statement, not a highlight reel.

**Multi-UID coding image (fixture R1).** `requires: {multi_uid: true}`. Host A: **reject before create**, `HOST_CAPABILITY_UNAVAILABLE` (HTTP 412), naming `identity.uid_mapping_mode` as the unmet capability. Host B: accept. On a multi-backend deployment the planner would route this spec to the Kernel backend instead of rejecting — the rejection shown here is the contract behavior for a single native host.

**512 MiB request vs hard limit (fixture R2).** `resources.memory_bytes: 536870912` **plus** `requires: {hard_memory_limit: true}`. Host A: **reject**, `HOST_CAPABILITY_UNAVAILABLE`. The fixture note is the rule: requesting 512 MiB does not create hardness, and falling back to an rlimit while claiming a cgroup limit is forbidden.

**Private `/proc` (fixture R3).** `requires: {private_proc: true}`. Host A's measured mode is `RBIND_RO_HOST_PROC` → **reject**. This is the requirement to set when argv/metadata visibility to co-located sandboxes is unacceptable for the workload.

**Seccomp on two different hosts (fixtures R4 / R4b).** `requires: {seccomp: true}` against a runc build *without* seccomp: **reject** — never silently run unfiltered. Same spec against host A's seccomp-capable build: **accept**. Capability truth is per-host, not per-product.

**Outbound network (fixture R5).** `requires: {outbound_network: true}` on host A (no rootless network helper): **reject**.

**Image `USER 1000`, default policy (fixture A2).** Host A: **reject** with `POLICY_UNSUPPORTED`. The default `STRICT` policy refuses ownership expressions the host cannot represent faithfully.

**Image `USER 1000`, explicit compatibility switch (fixture A3).** Same spec with `single_id_compatibility_policy: FLATTEN_ROOT`: **accept**, and the response must carry the degradation `identity.ownership_model` — image user folded to uid 0, disclosed. Compatibility modes exist, but they are opt-in and always visible in the result.

**Everything on host B (fixture A4).** Host B accepts the full set — illustrating that rejection today is a statement about *this host's measurements*, not about LightSandbox as a whole.

## The planner's three rules

1. **Requirement-driven, never runtime-driven.** `AUTO` walks NATIVE → KERNEL → MACHINE and takes the first backend observed SUPPORTED for every hard requirement; it does not score, guess, or "try and see".
2. **Reject pre-create, in the open.** Failures are `HOST_CAPABILITY_UNAVAILABLE` / `POLICY_UNSUPPORTED` with the specific capability named — returned *before* any resource is allocated.
3. **Explicit pin, explicit failure.** Pinning `backend_preference` to something unsatisfiable rejects the same way. The one thing the planner never does is switch your backend silently.

## Try the logic offline

The resolve-and-reject logic is embedded in the reference tooling — the environment planner exercises the same validate-then-plan discipline against an environment document:

```bash
python3 tools/lsbx.py --json env plan --file examples/native/environment.json
```

(Image references without digests surface in `unresolved`; unknown fields are refused.) The per-requirement outcomes above are pinned by the fixture cases and re-verified by the repo's resolver test suites on every change.

Related: `concepts/backends.md` decision table · `concepts/capabilities.md` mapping reference.
