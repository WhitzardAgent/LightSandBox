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
| CONFIG_MODULES | y | DECISION | see `module-policy.md` — production build should be OFF |
| CONFIG_CGROUPS | y | REQUIRED for resource semantics | guest cgroup2 mounts |
| CONFIG_CGROUP_SCHED / CPUACCT / BLK_CGROUP / DEVICE | y | OK | controllers `cpu io` observed |
| **CONFIG_MEMCG** | **NOT SET** | **REQUIRED FLIP** | guest `memory.max` unavailable (proven by resource spike: controllers lack `memory`); without it, guest-internal hard memory limits are NOT_TESTED |
| **CONFIG_CGROUP_PIDS** | **NOT SET** | **REQUIRED FLIP** | guest-internal `pids.max` (fork-bomb containment) unavailable |
| **CONFIG_UML_NET_VECTOR** | **NOT SET** | **REQUIRED FLIP (§34)** | no slirp/daemon/vde transport family; guest networking impossible on THIS binary |
| CONFIG_SLIP | **m** | TRAP | a module in a distribution-less initramfs = never loadable; exactly the module-policy lesson |
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
