# UML guest kernel — config artifact (round-04 §47)

This is the formal artifact for the UML guest kernel used by spike phases
A–F. The kernel BINARY stays pinned; this document classifies every config
symbol the product cares about and records the required flips discovered by
execution this round.

## Provenance and digests

- Version: Linux **6.19.14**, `ARCH=um` (User-Mode Linux)
- Source lock: `spike/uml/source-lock.json` (round-03 via TUNA mirror,
  `linux.tar.xz` sha256 `cde8bf6739be…ec6f185`; network note and PGP caveat
  recorded there)
- Binary (this build, phase-a, unchanged through phase-f):
  `spike/uml/work/src/linux/linux`, sha256 **`477a39b5012d…`** (full digest
  in the phase-b run logs), statically linked, initramfs-embedded
- Build host: the round-03 spike host; config = UML defconfig + deltas below

## Symbol classification (grounded in the actual `.config`)

| Symbol | State | Class | Consequence |
|---|---|---|---|
| CONFIG_HOSTFS | y | REQUIRED | builder materialization channel (restricted RO expose only) |
| CONFIG_BLK_DEV_UBD | y | REQUIRED | base disk + official COW (`ubd0=cow,backing`) |
| CONFIG_EXT4_FS | y | REQUIRED | base disk filesystem |
| CONFIG_DEVTMPFS(+_MOUNT) | y | REQUIRED | /dev nodes without pre-baked initramfs nodes |
| CONFIG_MCONSOLE | y | OPTIONAL | management socket only; NOT the exec path (phase-d) |
| CONFIG_MAGIC_SYSRQ | y | OPTIONAL | diagnostics |
| CONFIG_MODULES | y | DECISION | see `module-policy.md` — production build should be OFF — **c1: y → c2: NOT SET** (module count `=m` = 0 in candidate-2) |
| CONFIG_CGROUPS | y | REQUIRED for resource semantics | guest cgroup2 mounts |
| CONFIG_CGROUP_SCHED / CPUACCT / BLK_CGROUP / DEVICE | y | OK | controllers `cpu io` observed |
| **CONFIG_MEMCG** | **NOT SET** | **REQUIRED FLIP** | guest `memory.max` unavailable (proven by resource spike: controllers lack `memory`); without it, guest-internal hard memory limits are NOT_TESTED — **c1: NOT SET → c2: y** (built in kernel-candidate-2, see below) |
| **CONFIG_CGROUP_PIDS** | **NOT SET** | **REQUIRED FLIP** | guest-internal `pids.max` (fork-bomb containment) unavailable — **c1: NOT SET → c2: y** |
| **CONFIG_UML_NET_VECTOR** | **NOT SET** | **REQUIRED FLIP (§34)** | no slirp/daemon/vde transport family; guest networking impossible on THIS binary — **c1: NOT SET → c2: y**; note: in 6.19.14 this is the *only* UML net Kconfig symbol (no CONFIG_UML_NET / _SLIRP / _DAEMON exist) |
| CONFIG_SLIP | **m** | TRAP | a module in a distribution-less initramfs = never loadable; exactly the module-policy lesson — **c1: m → c2: NOT SET** (trap eliminated) |
| **CONFIG_SMP** | **NOT SET** | DECISION | guest is single-vCPU (proven: nproc=1); flip only with a multi-vCPU requirement — density plan currently assumes 1 |
| CONFIG_NET | y | NECESSARY-NOT-SUFFICIENT | base networking without UML transports |
| CONFIG_USER_NS | not set | OK | not needed inside the guest (host side stays rootless) |
| CONFIG_SECCOMP | y | OK | guest-internal seccomp available |

## Required next build ("kernel-candidate-2")

Flip: `CONFIG_MEMCG=y`, `CONFIG_CGROUP_PIDS=y`, plus §34's UML network
transport family (`CONFIG_UML_NET=y` + the selected transport, slirp
preferred for rootless). Keep: everything in REQUIRED. Decide: `CONFIG_SMP`
(stay off unless a multi-vCPU requirement lands), `CONFIG_MODULES=n`
(policy below). Re-evidence needed after rebuild: resource spike R3 must
move from NOT_TESTED to measured; §34 network legs become possible.

## kernel-candidate-2 (round-05)

Built 2026-09-27 per the round-05 directive §7. Everything above this line
remains the candidate-1 record; this section is the candidate-2 addendum.

- Build dir: `spike/uml/work/src/linux` (same tree, incremental over
  candidate-1 objects; build log `spike/uml/work/kernel-build-k2.log`)
- Binary: `spike/uml/work/src/linux/linux`, sha256
  **`084b8c3983f697aa1c89fcd1397e9bf7d234ca977e15cae32d8863cd7d8903ec`**
- Final `.config` sha256: **`242fb1e03dba1c5806b41ae7b9112080dc47f6e02fd097e5b3e7afe6321f621d`**
  (pre-flip c1 config preserved at `spike/uml/work/config-c1-preflip.txt`)
- version/kernelrelease: **6.19.14** (unchanged; `make -s ARCH=um kernelrelease`)
- Module count (`grep -c '=m$' .config`): **0**
- Flip grep verbatim:
  `CONFIG_MEMCG=y`, `CONFIG_CGROUP_PIDS=y`, `CONFIG_UML_NET_VECTOR=y`,
  `# CONFIG_MODULES is not set`, `# CONFIG_SLIP is not set`
- Net family chosen: **`CONFIG_UML_NET_VECTOR=y`** — in 6.19.14 this is the
  sole UML-net Kconfig symbol: `CONFIG_UML_NET`, `CONFIG_UML_NET_SLIRP` and
  `CONFIG_UML_NET_DAEMON` do not exist in this tree (no `slirp` string under
  `arch/um/`), so olddefconfig dropped them as unknown; `.config` was NOT
  hand-edited. In-tree vector transports: raw, fd, vde (user side), tap,
  gre, l2tpv3, hybrid, bess. Rootless slirp-style egress needs the `fd`
  transport bound to a host-side slirp4netns/passt socket — §34 follow-up.
- olddefconfig surprise (recorded verbatim in
  `spike/results/uml-kernel-candidate-2.json` → `eds_olddefconfig_deltas`):
  **`CONFIG_STATIC_LINK` was dropped** — `UML_NET_VECTOR` selects
  `MAY_HAVE_RUNTIME_DEPS`, which STATIC_LINK's `depends on
  !MAY_HAVE_RUNTIME_DEPS` forbids. Candidate-2's `linux` is therefore
  **dynamically linked** (interpreter `/lib64/ld-linux-x86-64.so.2`), unlike
  statically-linked candidate-1. Any chroot/static-image assumption about
  the uml binary is void for candidate-2; host glibc is now required at
  guest-kernel exec time (true on this spike host; smoke proves it runs).
- Build environment defect fixed (kept for posterity): the agent-shell
  `/root/.minimax/shims/rm` prints trash notices on stdout; kbuild
  `try-run`/`cc-option` capture that into gcc argv and the build dies at
  `arch/x86/um/user-offsets.s`. Fixed with a stdout-silent wrapper at
  `spike/uml/work/tools/bin/rm` routing to the same sanctioned
  `mavis-trash` binary. Two failed attempts recorded in the JSON.
- Build wall time: 2m24s (20:13:47Z→20:16:11Z, `-j4`, host load ~38).
- Smoke (same invocation both binaries, `timeout 60 ./linux mem=256M
  con0=fd:0,fd:1`): marker NAME sets identical (12/12), no panic/BUG lines
  in either run, host rc=0 both (guest poweroff, not timeout).
  `LSB_CGROUPS` value moved 6 → 7; `memory_cgrp_subsys` and
  `pids_cgrp_subsys` both present in the new `System.map`.
  Verdict: **SMOKE_OK**. Console: `evidence/runtime/uml/phase-k2-smoke.txt`;
  pre/post logs in `spike/uml/work/k2-{pre,post}smoke.log`.
- **Re-evidence: resource retest DONE, verdict `RESOURCE_SEMANTICS_MISMATCH`
  (15 MATCH / 4 MISMATCH, 2026-09-28 recorded as
  `spike/results/uml-resource-semantics-candidate2.json`).** Memory:
  `memory.max=32MiB` enforced (allocator 137, init+channel survive) —
  GuestWorkloadHardLimit for memory is PROVEN on candidate-2. pids:
  enforcement UNPROVEN — `pids.current` marker never reached the host
  (measurement indeterminate, guest survived; hardened capture pending).
  cpu: `cpu.max` write rc=1 and no throttling of a 3 s hog (~3.03 s CPU
  used, 50% cap would allow ≤2.4 s). Leading hypothesis: no `scripts/config`
  flip ever touched `CONFIG_FAIR_GROUP_SCHED`/`CONFIG_CFS_BANDWIDTH` in
  `build.sh`, and UML defconfig defaults them off — `cpu` shows in
  `cgroup.controllers` but the `cpu.max` knob rejects writes. Pending
  `grep CFS_BANDWIDTH` on the worker `.config` + verbose rerun.
  R4b shmem oversubscribe: divergent (137 / channel loss / guest SIGABRT)
  — tmpfs is host-charged; oversubscribe can destabilize the whole guest;
  recorded honestly, not a stability guarantee. Module policy negative
  probes MATCH (`=m` 0). The §34 network legs remain pending.
- **kernel-candidate-3 (planned, worker-gated):** flip
  `CONFIG_FAIR_GROUP_SCHED=y` + `CONFIG_CFS_BANDWIDTH=y` if grep confirms
  absence; re-run the resource suite with the hardened
  `run-resources-c2.py` (unsilenced subtree_control enables, cpu.max
  existence/verbose-write probes, R4b per-attempt cookie + console tail).
