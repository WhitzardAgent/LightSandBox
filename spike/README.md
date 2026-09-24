# Portable Rootless Runtime Spike(feasibility-only)

**This directory is throwaway feasibility code. It is NOT the production
runtime, NOT wired into `crates/`, `contracts/`, CI product gates, or the
pack catalog. No production claim may quote it without the ADR and the T41
gate decision.**

Goal: answer, with real Linux runtime behaviour on this representative
rootless VM, *how complete a native sandbox we can honestly stand up*.

- Toolchain: statically built locked sources (`/opt/toolchains/lightsandbox/bin`,
  see `specs/upstream-lock.json`: runc `41b74772b651`, umoci `f5d1219acaf6`,
  CGO_ENABLED=0, seccomp off — host lacks libseccomp headers).
- Local state (rootfs/bundles, big files): `/tmp/lsbx-spike` (gitignored,
  recreated by `lab.py`; must be reachable by uid 65534, hence not `$HOME`).
- All sandbox processes run as the real unprivileged user (uid 65534), never
  as the nested layer's uid 0 — see `firstrun.json` for why the rootful path
  dies on the read-only cgroupfs here.
- Committed evidence: `spike/results/*.json`.

Rules: argv only (no shell composition in measurements); failed probes are
kept verbatim (command, exit code, stderr/stdout); no test oracle is modified
to pass; PRoot is only evaluated if /proc or multi-UID becomes a real blocker;
network helpers are optional and never block `network=none` as the honest v1
default.

## Experiments and verdicts (2026-09-24)

| Experiment | Command | Evidence | Verdict |
|---|---|---|---|
| fixtures A/B (from scratch, no docker) | `python3 spike/lab.py fixtures` | `results/fixtures.json` | built; A single-user + python/gcc/git/node, B multi-uid + setuid + device node + USER 1000:1000 |
| umoci unpack --rootless metadata | `python3 spike/lab.py unpack` | `results/unpack.json` | ownership flattens 0:0 (original uids in xattrs); setuid preserved; device → empty file + warning |
| first real execution + /proc shapes | `python3 spike/lab.py firstrun` | `results/firstrun.json` | rootful path EROFS; P0 fresh proc EPERM; runc≥1.4 rejects no-/proc; **rbind-ro host /proc shape executes** |
| P1 real programs | `python3 spike/lab.py p1` | `results/p1.json` | bash/python3(pid1)/git/node/gcc -O2 all work; /proc/self→host pid; ~33 host pids visible |
| P2 cross-exposure SECURITY EXPERIMENT | `python3 spike/lab.py p2` | `results/p2.json` | cmdline+fd-count visible; **environ EPERM (no secret leak)**; signal/ptrace ESRCH |
| single-ID multi-uid | `python3 spike/lab.py uids` | `results/uids.json` | SUPPORTED 5 / EMULATED setuid / **BROKEN chown-to-unmapped, su, USER 1000** (recorded, not flattened) |
| rootfs paths | `python3 spike/lab.py rootfs` | `results/rootfs.json` | FULL_COPY works (≈0.9 s/972 MB, persistent, 0.2 s delete); TMPFS_COW mechanism proven; supervisor block required |
| A/B isolation matrix | `python3 spike/lab.py isolation` | `results/isolation.json` | 13 axes; 0 INTERFERABLE; 3 metadata axes VISIBLE_READONLY via shared /proc |
| performance baseline | `python3 spike/lab.py perf` | `results/perf.json` | prepare 0.8–1.0 s; create+exec 52–185 ms (warm ~55–75 ms); delete 187 ms; idle RSS 272 kB; 0 resident runc supervisor |

**Decision**: see `docs/adr/0001-portable-rootless-runtime.md` — option B
(native rootless runc with explicit degraded capabilities); T41 gate PASSED.
