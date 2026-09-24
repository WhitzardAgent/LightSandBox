# ADR 0001 — Portable Rootless Runtime backend for v1

- Status: **Accepted** (evidence-based, 2026-09-24); **Amended A1** (P2.1 /proc confidentiality matrix, 2026-09-24 Round-02 — see §7)
- Scope: first-host deployment target `PORTABLE_ROOTLESS` (HostCapabilityTier, docs/v5/03_DOMAIN.md)
- Evidence root: `spike/results/*.json` (feasibility code in `spike/`, NOT merged into `crates/`, `contracts/` or CI); security evidence duplicates: `evidence/security/`

## 1. Decision

> **v1 Portable Rootless backend = option B: native rootless runc is viable
> with explicit, contract-visible degraded capabilities.**

The native path works on the most common no-root VM shape (nested container,
uid-0-in-userns only, single-ID mapping, no subuid, no cgroup delegation, no
/dev/fuse, private procfs EPERM). All required benchmark workloads
(bash/python3/gcc/git/node) execute with correct sandbox identity
(`id` = uid 0 inside, host uid 65534 outside), correct cwd/env, dedicated
PID/IPC/UTS/network/mount namespaces, and a measured cross-sandbox isolation
matrix with **zero interference axes**.

Option B carries explicit degradations (§4). They are recorded in the
HostReport (`evidence/host/hostreport-v1.json`) and must surface in the
native contract — never silently flattened.

## 2. Options considered

| Option | Verdict |
|---|---|
| A. Native rootless runc viable (full parity) | **Rejected** — multi-UID semantics, hard cgroup limits, private /proc are measurably unavailable; claiming A would be dishonest |
| **B. Native rootless runc, explicit degraded capabilities** | **Chosen** |
| C. Need alternative native launcher | Not needed for execution; a thin **per-sandbox resident supervisor** (contained in runc's model) is required as an architecture block for lifecycle + TMPFS_COW wiring — it complements runc, not replaces |
| D. Need optional PRoot backend | Trigger condition not met: neither `/proc` nor multi-UID became a blocker for the benchmark-parity scope. PRoot emulates UID semantics without kernel truth; it would hurt gdb/ptrace Cyber workloads. Locked (`specs/upstream-lock.json`, GPL-2.0-or-later) as a possible future compatibility backend only |
| E. Common host cannot safely support multi-sandbox workloads | Disproven by `isolation.json`: 13 axes, 0 INTERFERABLE |

## 3. Measured facts (each with evidence file)

- **P0 fresh private procfs**: EPERM at `mount(proc)` inside user+pid+mount ns (`firstrun.json`); confirmed layer = outer container policy (rootless-host-report.md), not runc.
- **runc ≥ 1.4 procfs safety net**: a bundle *without any /proc* is rejected (`unsafe procfs detected`, `firstrun.json`). Only two shapes exist: fresh proc mount (EPERM here) or rbind-ro host `/proc`. Second shape works and passed all workload tests.
- **P1 workloads** (`p1.json`): bash ✓, python3 (pid 1 in own PID ns) ✓, git ✓, node v22 ✓, gcc -O2 compile+run ✓. `/proc/self` resolves to the *host* pid; 33–35 host PIDs visible as metadata (measured samples: 33, 35).
- **P2 cross-exposure SECURITY EXPERIMENT** (`p2.json`): from sandbox B, A's host pid shows `cmdline` and fd-count, but **`environ` is EPERM**, `environ` content did not leak; `kill(pid,0)/SIGTERM` → ESRCH; `PTRACE_ATTACH` → ESRCH. A remained running. *(Refined by P2.1 below: the earlier note "processes in another userns are non-dumpable" is NOT what was measured — the same deny pattern holds even for a same-uid, non-userns control; see §3 P2.1 and §7.)*
- **P2.1 /proc confidentiality matrix, marker-proven** (`spike/results/p21.json`, canonical copy `evidence/security/proc-confidentiality.json`, 2026-09-24 Round-02): sandbox B and a same-uid dumpable control both carried random `LSBX_SECRET_<uuid>` markers in argv/env/cwd/open-file/unix-socket-path; sandbox A (same host uid 65534, own userns+pidns, rbind-ro host /proc) attempted reads.
  - **`/proc/<pid>/cmdline`: CONTENT_VISIBLE, full argv (373 B incl. marker) readable CROSS-SANDBOX** → proc content confidentiality is NOT guaranteed; argv must be treated as PUBLIC to co-located sandboxes.
  - `environ` / `cwd` / `root` / `maps` / `io` / `stack`: ACCESS_DENIED (errno 13); `syscall`: EPERM — mm content does NOT leak cross-sandbox.
  - `fd` DIRECTORY is listable (count = metadata); every fd readlink → errno 13 (fd targets do NOT leak).
  - `status` / `stat` / `mountinfo` / `wchan`: readable metadata (no secret markers by construction).
  - Identical deny/allow pattern for a same-uid, non-userns control process ⇒ payload dumpability is NOT the discriminator. **Mechanism caveat**: this host runs `kernel.yama.ptrace_scope=1`, which independently denies these reads between non-descendant same-uid processes; the reader-confinement (userns + procfs per-file checks) and Yama mechanisms CANNOT be discriminated on this host (a ptrace_scope=0 host would be needed). All ACCESS_DENIED rows are host-profile facts, not a portable proof of which layer denies. Direct dumpable-flag readback was inconclusive (no user-visible flag in `/proc/<pid>/status`); recorded as inconclusive method, not silently dropped.
- **single-ID multi-UID** (`uids.json`): SUPPORTED = id/ls/cat/chmod on flattened files; EMULATED = setuid bit (root==outer uid); **BROKEN = chown→unmapped uid (EINVAL), su→app user, image USER 1000:1000 (runc rejects unmapped uid)**. No silent USER→0 rewriting anywhere.
- **Rootfs** (`rootfs.json`): FULL_COPY works (prepare ≈0.8–1.0 s / 972 MB, persistence across starts, delete ≈0.2 s). TMPFS_COW mechanism proven via mount(2) inside the sandbox (tmpfs scratch 8 MB write, lazy copy 5 ms/5.9 MB, bind-ro self-mask EROFS, 32 MB tmpfs write ⇒ host MemAvailable −62 MB live — raw single-sample delta; the payload also held a 32 MB python buffer at measure time, so Δ ≈ 2× the tmpfs share and the causal attribution is approximate) — runtime wiring requires the supervisor architecture block; disk-overlay deliberately excluded.
- **Isolation matrix** (`isolation.json`, Round-02 re-measured with per-axis positive controls): filesystem / signals / ptrace / proc-environ / TCP loopback / abstract-unix / SysV shm / hostname = ISOLATED; **host loopback = NOT_TESTED on this host** (no 127.0.0.1:22 listener exists; Round-01's 'ISOLATED' verdict for that axis was vacuous and is withdrawn, not confirmed); process-metadata / proc-cmdline (FULL argv content, per P2.1) / proc-fd-count = VISIBLE (read-only); 0 axes INTERFERABLE or REACHABLE. **Integrity isolation is strong; content confidentiality is NOT provided for argv.**
- **Performance baseline** (`perf.json`, untuned): rootfs prepare FULL_COPY 820–928 ms (972 MB; 3 samples: 820/890/928); create+exec `/bin/true` 52–185 ms (warm ≈ 52–75 ms); create+exec python3 70–155 ms; delete 187 ms; idle payload RSS 272 kB, ppid 1, **0 resident runc supervisor processes** (detached mode reaps nothing).
- **umoci rootless unpack** (`unpack.json`): ownership flattens to 0:0 (original uid kept in 4 xattrs), setuid bits preserved, device nodes become empty files with a warning (fixture B `/dev/null`).

## 4. Explicit degraded capabilities (contract-visible)

1. **HostCapabilityTier = PORTABLE_ROOTLESS** only; no claim of FULL_ROOTLESS/ROOTFUL parity.
2. **UID/GID**: single-ID mapping; container uid 0 ↔ host uid. Multi-UID features (su/sudo/USER≠0/unmapped chown) are BROKEN and must be reported as such. `SingleIdCompatibilityPolicy` is an explicit benchmark-mode switch, not hidden behavior.
3. **cgroup**: no writable delegation ⇒ **HardResourceLimits = UNSUPPORTED**. RLIMIT_* are process-level guardrails only; RLIMIT_NPROC counts per host-uid and MUST NOT be sold as per-sandbox pids quota.
4. **/proc policy (layer N, versioned; amended by P2.1)**: default = rbind+ro+nosuid+noexec+nodev of host `/proc`.
   - **Contract field `proc_isolation = METADATA_AND_ARGV_VISIBLE`** — measured matrix (`evidence/security/proc-confidentiality.json`):
     - CONTENT_VISIBLE cross-sandbox: full `cmdline` (argv incl. inline `-c` code); per-pid `status`/`stat`/`mountinfo`/`wchan` metadata; host PID enumeration; fd DIRECTORY count.
     - ACCESS_DENIED cross-sandbox: `environ`, `cwd`, `root`, `maps`, `io`, `stack`, fd symlink targets, `syscall` (EPERM).
   - Integrity (no signal/ptrace/write interference) remains fully evidenced (`isolation.json`, 13 axes, 0 INTERFERABLE); **content confidentiality is NOT provided for argv**.
   - **Trust-model consequence**: PORTABLE_ROOTLESS v1 workers are **single-trust-domain** workers ("tenant" = security trust domain; a worker still runs many sandboxes). Hostile multi-tenant confidentiality is NOT claimed at this ProcPolicy level.
   - **Engineering rule (mandatory, see `docs/security/SECRET_HANDLING.md` LSR-1)**: secrets, API keys, credentials, hidden-verifier tokens MUST NOT be passed via argv (nor inline `-c` source) to sandbox payloads. Env is currently mm-content-denied on THIS host, but is NOT contract-guaranteed until a contract-guaranteed confidential channel exists; preferred channels = credential proxy, passed Unix FD, protected file with 0400-in-sandbox ownership.
   - Upgrade path = private procfs the moment the deployment layer permits it (kernel/outer-policy), or a supervisor-provided pidns-scoped proc. The policy is a versioned contract field, so upgrades do not change semantics silently.
5. **seccomp**: static runc built without libseccomp (host has no headers) ⇒ no syscall filtering; mitigations = userns, minimal capability set, noNewPrivileges, read-only /proc bind. To be revisited on tier elevation.
6. **network**: default `network=none` realized as private netns with loopback only (cross-sandbox TCP and abstract-unix measurably unreachable; the host-loopback axis is NOT_TESTED on this host — no loopback service exists to probe against). Outbound TCP/DNS via pasta/slirp4netns = optional future work, deliberately out of the critical path.
7. **Supervision**: runc detached mode leaves **no resident supervisor** (payload reparents to PID 1). LightSandbox's per-sandbox lightweight supervisor is a required architecture block for lifecycle observation/exec-attach/cleanup/TMPFS_COW — consistent with the v5 "no implicit replay after lost execution results" rule.

## 5. T41 entry gate (the 8 required proofs)

| Gate item | Result | Evidence |
|---|---|---|
| Real isolated workload launch | ✅ python/gcc/git/node/bash run as root-in-userns | `firstrun.json`, `p1.json` |
| Rootfs isolation | ✅ separate copies + mount ns; write-marker test | `isolation.json` |
| Lifecycle supervisable | ⚠️ admitted gap → supervisor architecture block required pre-T04 runtime claim (gate passed with block recorded, no fake success) | `perf.json` idle step |
| No obvious A/B cross-control | ✅ signals/ptrace/shm/net/sockets all ESRCH/EINVAL/refused | `p2.json`, `isolation.json` |
| Cleanup completes | ✅ delete 187–196 ms as sandbox uid | `rootfs.json`, `perf.json` |
| /proc policy decided | ✅ §4.4 versioned policy with measured exposure; confidentiality class = METADATA_AND_ARGV_VISIBLE (amended by P2.1) | `firstrun.json`, `p2.json`, `p21.json` |
| single-ID limits explicit | ✅ SUPPORTED/EMULATED/BROKEN classified | `uids.json`, `unpack.json` |
| no-cgroup behavior explicit | ✅ UNSUPPORTED hard limits; RLIMIT guardrail language locked | host report, `perf.json` note |

**Gate verdict: PASS for entering T41.** The ⚠️ supervision item is passed as an
explicit architecture block (recorded, not worked around), matching the
repository rule that mock/dry-run does not prove anything.

## 6. Consequences

- T41 (native contract freeze) may proceed; the contract must encode the §4 degradations as first-class fields (tier, proc policy incl. its confidentiality class, single-ID policy, resource-limit capability).
- T41 must also encode: (a) requirement-vs-observation separation — requests for unavailable capabilities (multi_uid, hard limits, private_proc, seccomp, outbound net) REJECT pre-execution with `HOST_CAPABILITY_UNAVAILABLE`, never silently degrade; (b) the SECRET_HANDLING LSR-1 rule as a contract comment + HostCapabilities field semantics.
- T04 (runtime core) is gated on the supervisor architecture block, not on any re-run of this spike.
- Spike code remains in `spike/` (feasibility-only); production lessons flow into the contract and runtime design, not by merging spike code.
- If a future host probes FULL_ROOTLESS, this ADR's `/proc`, seccomp, cgroup and UID sections are revisited tier-by-tier.

## 7. Amendment log

- **A1 (2026-09-24, Round-02)**: /proc confidentiality re-measured by the dedicated P2.1 experiment (random-marker method, cross-sandbox same-uid readers + same-uid dumpable control). Finding: **full argv content is cross-sandbox CONTENT_VISIBLE; environ/mm-content is ACCESS_DENIED**. Round-01's shorthand that could be read as "secrets do not leak" is corrected: it held only for environ/mm content, never for argv. `proc_isolation` contractual class set to `METADATA_AND_ARGV_VISIBLE`; single-trust-domain worker model made explicit; SECRET_HANDLING rule LSR-1 added (`docs/security/SECRET_HANDLING.md`). Integrity results unchanged.
- **A2 (2026-09-24, Round-02, independent-review follow-ups)**: (a) P2.1 mechanism attribution narrowed: with `kernel.yama.ptrace_scope=1` on this host, the ACCESS_DENIED rows cannot be attributed to userns reader-confinement specifically — Yama independently denies the same reads; the matrix is a host-profile fact, mechanism undiscriminated. (b) Padded numeric ranges replaced by measured sample ranges (rootfs prepare 820–928 ms; visible host PIDs 33–35). (c) TMPFS_COW MemAvailable −62 MB re-labelled as raw single-sample delta with an explicit ≈2× confound (payload held a 32 MB python buffer while writing 32 MB to tmpfs). (d) `isolation.json`/`p2.json` regenerated with positive controls per axis (same-netns client must CONNECT, B-side marker write must rc=0, same-IPC-ns shmat must succeed, host sshd presence checked before the host-loopback axis), evidence-based classifiers (ISOLATED only on EACCES/errno evidence; crashed probes classify INCONCLUSIVE), P2 sleep-probe argv[0] strict matching (previous substring match self-matched the probe source), and explicit single-sample notes. Isolation conclusions unchanged: 0 axes INTERFERABLE/REACHABLE.
