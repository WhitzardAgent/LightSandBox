# Security model

This page lists what LightSandbox isolates, what it does not, and the rules you must follow when handling secrets. Every claim is a measured host-profile fact, and host-profile facts can differ on your host — the contract carries them per host rather than generalizing them.

## Native backend (rootless runc)

What is true on the measured portable-rootless host class:

- **Shared host kernel.** All sandboxes and the host share one kernel. A sandboxed process is a set of namespaces and a single-ID user namespace, nothing more.
- **Single-ID user namespace.** Container uid 0 maps to one unprivileged host uid. Multi-UID features (`su`, image `USER` ≠ 0, `chown` to unmapped users) are rejected or explicitly disclosed as degradations — never silently faked.
- **`/proc` policy.** Sandboxes receive a read-only bind of the host `/proc`. Measured consequence: process **metadata and full argv are visible** to co-located sandboxes (`cmdline`, `status`, `stat`, fd counts, PID enumeration), while `environ`, memory maps, fd targets, and credential files are **access-denied on this host** (see the attribution caveat below).
- **No guest hard limits from the host.** No cgroup delegation exists on this host class, so the host enforces no per-sandbox memory/cpu/pids ceiling. RLIMIT settings are process-level guardrails only and are never represented as cgroup hard limits.
- **Single-trust-domain assumption.** Portable-rootless workers are single-trust-domain workers: many sandboxes, one tenant. Hostile multi-tenant confidentiality is not claimed at this `/proc` policy level.

!!! note "Host-profile fact, with a mechanism caveat"
    The `environ`/memory denies above were measured on a host with Yama `ptrace_scope=1`, which independently blocks those reads between same-uid processes. On a differently configured host the same reads might succeed. Treat the deny list as a fact about the measured host profile, not a portable guarantee.

## Kernel backend (User-Mode Linux)

- Each sandbox runs its **own guest kernel**: own `/proc`, real guest UIDs, guest cgroup tree, guest DAC enforcement (uid-based file access inside the guest measurably works).
- A guest kernel is **weaker than hardware virtualization** — upstream UML documentation is explicit about this. A determined guest-kernel exploit has a softer boundary to the host than a VM escape would face.
- **Hardening roadmap** (planned, not shipped): no arbitrary hostfs mounts (restricted read-only hostfs only for the controlled image builder), module loading disabled in the production kernel, UML running as an unprivileged host process, seccomp and namespacing around the UML process where the host permits.

## What LightSandbox protects today

Mechanisms that exist and are tested in the current codebase:

- **Default-deny planning.** Environment plans and sandbox specs validate against frozen schemas with unknown fields rejected; requirements the host cannot satisfy are rejected pre-create (`HOST_CAPABILITY_UNAVAILABLE`), never silently degraded.
- **Secret redaction in client output.** `exec --dry-run` replaces every `--env` value with `<redacted>`, so request previews are safe to share.
- **Fail-closed parsing.** Contract and frame parsing rejects malformed input (strict JSON, no NaN, size caps) instead of guessing.
- **Explicit outcome accounting.** Lost execution results surface as exit record `unknown` — the system never invents exit code 0.

## What LightSandbox does not protect

- **Host-kernel exploits against the Native backend.** Sandboxes share the host kernel; a kernel vulnerability is a boundary-crossing vulnerability. Seccomp filtering is a host capability (available on the measured host) and sandbox policies must request it explicitly — it is never assumed.
- **Guest-to-host escape on the Kernel backend** beyond the guest-kernel boundary described above.
- **Secrets you pass in argv.** On the Native backend, argv of any sandbox process is readable by co-located sandboxes. **Never pass secrets, tokens, or hidden-verifier material on a sandbox command line** — including inline `sh -c` source. Prefer a credential proxy, a passed file descriptor, or a file with `0400` permissions owned in-guest.
- **Secrets in env, durably.** `environ` reads were denied on the measured host (see the caveat above), but no contract-guaranteed confidential channel exists yet. Do not treat env vars as a secrets mechanism on shared workers.
- **Malicious images.** An image you materialize runs its payload inside the isolation boundary described here — the boundary does not audit what the payload intends.

## Rules to operate by

1. Treat portable-rootless workers as **single-trust-domain**.
2. Keep secrets out of argv (and out of env on co-located workers).
3. Pin images by digest; the planner flags unresolved references instead of pulling tags.
4. When a workload needs confidentiality the Native backend cannot offer, require `private_proc` — the planner will reject or escalate rather than fake it.
