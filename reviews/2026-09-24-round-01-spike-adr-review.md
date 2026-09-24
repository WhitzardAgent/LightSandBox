# REVIEW REPORT (round-01 spike + ADR)

Reviewer: fresh-context verifier subagent (harness-level different-context review; NOT an independent organization security assessment)
Commit reviewed: `2138549`(spike 证据 + ADR 0001;ADR 以 HEAD 为准交叉核对)
Date: 2026-09-24

## Verdict

**MAJOR** for commit 2138549. Core decision (option B, PORTABLE_ROOTLESS with degraded capabilities) is honestly evidenced — every BROKEN/EPERM failure is recorded verbatim with nonzero rc and stderr, and tool sha256s in evidence match the live binaries. But one security-boundary claim in the Accepted ADR (environ "NOT exposed" + "kernel dumpability" mechanism, plus "fd targets' content" not exposed) exceeds the committed evidence: the p2 design on this host (`kernel.yama.ptrace_scope=1`, verified live) cannot discriminate dumpability from Yama, and fd-target readability was never measured at commit time.

## Evidence-vs-claim table

| Claim | ADR/lab source | Evidence (file + key) | Verdict |
|---|---|---|---|
| isolation: 13 axes, 0 INTERFERABLE/REACHABLE | ADR:42; lab.py:1042-1044 | isolation.json steps[3] result (13 axis objects) | SUPPORTED |
| per-axis: filesystem/signals/ptrace/proc-environ/TCP-loopback/host-loopback/abstract-unix/shm/hostname ISOLATED | ADR:42 | isolation.json per-axis classifications(见 finding #4,#8 注释) | SUPPORTED (with notes) |
| perf prepare FULL_COPY 820–970 ms / 972 MB | ADR:43 | perf.json rootfs prepare runs_ms=[928,890,820], bytes=972170603 | PARTIAL — samples are 820–928; upper bound 970 has no sample |
| create+exec /bin/true 52–185 ms (warm 52–75) | ADR:43 | perf.json runs_ms=[52,185,163,58,75], exits all 0 | SUPPORTED |
| create+exec python3 70–155 ms | ADR:43 | perf.json runs_ms=[95,155,70], exits all 0 | SUPPORTED |
| delete 187 ms; T41 "187–196 ms" | ADR:43,67 | perf.json delete ms=187; rootfs.json FULL_COPY delete ms=196 | SUPPORTED |
| idle RSS 272 kB, ppid 1, 0 resident runc supervisors | ADR:43 | perf.json idle step VmRSS 272 kB, ppid "1", resident_runc_supervisors=0 | SUPPORTED |
| uids SUPPORTED x5 / EMULATED setuid / BROKEN x3 | ADR:40 | uids.json: 5×exit 0; setuid 0; chown exit 1 "Invalid argument"; su exit 1 abort; USER-1000 exit 1 runc user setup error | SUPPORTED — failures are genuine, not fake success |
| p2 environ EPERM, secret did not leak; kill/ptrace ESRCH; A survives | ADR:39 | p2.json probe stdout verbatim | SUPPORTED as measured fact — but target was a same-host-uid (65534) cross-sandbox process; mechanism confounded (finding #1) |
| §4.4 "NOT exposed: environ (kernel dumpability), fd targets' content" | ADR:53 | committed evidence: environ EPERM measured; **fd-target content/readlink never attempted** (only FD_COUNT) | PARTIAL / NOT_VERIFIABLE for fd targets — see finding #1 |
| P1 workloads all run; python is pid 1; /proc/self→host pid; 33–36 host pids | ADR:38 | p1.json: all exit 0; "self-> 4137268"; "visible host pids: 33" (isolation.json: 35) | SUPPORTED except "36" upper bound has no sample (finding #3) |
| rootfs FULL_COPY prepare ~0.9s/972MB, cross-start persistence, delete ~0.2s; TMPFS_COW 8MB scratch, 5ms/5.9MB lazy copy, EROFS mask, 32MB tmpfs⇒MemAvailable −62MB | ADR:41 | rootfs.json rows match (two `runc run` invocations both printing marker-3567 = real cross-start persistence; delta 62864 kB) | SUPPORTED (mem-impact attribution nit, finding #9) |
| umoci unpack flattens 0:0, original uids in 4 xattrs, setuid preserved, device→empty file + warning | ADR:44 | unpack.json B: owners {"0:0":55}, entries_with_xattrs=4, su mode 0o4755, stderr "rootless{dev/null} creating empty file in place of device 1:3" | SUPPORTED |
| P0 fresh proc EPERM; runc≥1.4 rejects no-/proc; rbind-ro host /proc shape executes | ADR:36-37 | firstrun.json: rootful EROFS exit 1; P0 mount EPERM; 4× "unsafe procfs detected …0x794c7630" exit 1; 4× bindproc exit 0 | SUPPORTED, verbatim |
| fixtures built from scratch, no docker | README:31; lab.py:371 | fixtures.json umoci history created_by "umoci insert"/"umoci config"; committed lab.py contains no docker invocation | PARTIAL — build transcript (umoci argv/rc) discarded by harness; claim rests on code + image metadata |

## Findings

**Blocker: none.**

**Major:**

1. **environ-confidentiality claim exceeds committed evidence; mechanism attribution unverified and confounded on this host.** ADR:39 "`environ` is EPERM (processes in another userns are non-dumpable)"; ADR:53 "NOT exposed: environ (kernel dumpability), fd targets' content". Facts: (a) this host runs `kernel.yama.ptrace_scope=1` (verified live), which blocks non-descendant same-uid /proc/environ reads independently of dumpability — the p2/isolation design cannot discriminate the two mechanisms; (b) no committed control (same-uid dumpable non-userns process) exists at 2138549; (c) fd-target *content* was never probed (only fd count); (d) payload self-setting PR_SET_DUMPABLE on a ptrace_scope=0 host is an unexplored residual. The observable EPERM is solid; the generalization into §4.4 design property + causal mechanism is not. Fix: reword ADR:39/53 to measured-fact scope or add the control evidence to the spike results and re-verify.

**Minor:**

2. **P2 probe self-match.** lab.py:631 `if b'sleep' in cl:` — the probe's own `-c` cmdline contains "sleep"; p2.json sleep_procs_visible lists the probe itself alongside A's real pid. Directed probes use the pid-file pid, so verdicts unaffected. Fix: match `^/bin/sleep` or exclude own host pid.

3. **Padded numeric upper bounds without samples.** ADR:43 "prepare 820–970 ms" (samples 820/890/928); ADR:38 "33–36 host PIDs" (samples 33, 35). Fix: report measured ranges or mark "≈".

4. **isolation.json proc_environ_of_A row has empty `detail`, and the classifier treats any probe failure as ISOLATED.** lab.py:989-990: `"VISIBLE" if leaked else "ISOLATED"` — a crashed probe records ISOLATED; detail "" discards stderr. Verdict corroborated by p2.json, but the predicate fails open toward the good verdict. Fix: require stderr match of Permission/EACCES for ISOLATED and record stderr.

5. **Filesystem axis discards the B-side marker-write result.** lab.py:960 write result never checked; had it silently failed, A's failing `cat` would (mis)confirm isolation. Fix: record the write step and require exit 0.

6. **sysv_shm classifier tautology.** lab.py:1024 `or "rc 0" not in r["stdout"]` makes blocked=True for any crashed/empty probe (fail toward ISOLATED). Evidence shows genuine errno 22, but no positive control that the same probe succeeds within one IPC ns.

7. **Missing negative/positive controls (committed evidence).** No shm same-namespace baseline; NET_SRV/ABS_SRV bind success never asserted (crash → "refused" → ISOLATED); `network_host_loopback` 127.0.0.1:22 vacuous if no host sshd listens — no listener control; no setns/nsfs axis (untested, also unclaimed).

8. **Single-sample functional/security probes.** No repetition parameter; every P1/P2/uids/isolation probe ran once; "single-sample" risk noted nowhere. Timing steps are repeated 3–5× (good).

9. **TMPFS mem-impact attribution.** ADR:41 "32 MB tmpfs ⇒ host MemAvailable −62 MB live": MNT_MEM_PY allocates a 32 MB python bytes object AND writes 32 MB to tmpfs, so delta ≈ 2× the tmpfs share; single sample. Honest as raw delta; the causal arrow overstates.

10. **Worktree drift during review (outside commit scope, flagged for integrator).** Mid-review uncommitted `M spike/lab.py`(+195 lines, p21), `?? spike/results/p21.json`, `?? evidence/security/` appeared; excluded from verdict (committed lab.py byte-identical to HEAD for reviewed sections). Re-run housekeeping after landing.

11. **Fixture build transcript not captured.** `run_forcing` discards umoci init/new/insert argv+rc; "no docker" is provable from harness code + umoci history metadata, not from a runtime transcript.

## Answers to angles 1-6 verified clean

1. **Evidence-vs-claim**: 13 isolation axes enumerated match JSON verdict exactly; uids BROKEN x3 real failures; perf numbers supported except padded bounds; rootfs persistence genuine cross-start; TMPFS rows match; unpack 4 xattrs/setuid/device-warning verbatim; firstrun EPERM/"unsafe procfs" present with rc; evidence timestamps precede commit.
2. **Harness honesty (committed lab.py 1-1046)**: `run`/`run_as` argv-only, verbatim capture incl. failures; `run_forcing` raises on nonzero; no swallowed rc, no retry/timeout-reclassification; no marker leak path (MTX secret only ever enters A's config written after B's bundle copy); classifiers for signals/ptrace/net/abstract/hostname fail toward INTERFERABLE/REACHABLE/SHARED on crash (conservative); exceptions are #4/#6. Detached stdio to log files with documented rationale.
3. **Unsafe shell**: no `shell=True`; bash -c payloads fixed strings; f-string interpolation only of ints/repr(str(int))/constant Paths — no injection surface. No `spike/payload.sh` exists; payload code is inline. NET/ABS sockets in separate netns — no tx/rx self-taint.
4. **Missing controls** — not clean; see findings #6,#7,#8.
5. **ADR honesty beyond finding #1**: seccomp absence explicitly disclosed with mitigations; no-cgroup/RLIMIT_NPROC warning honest; supervision gap admitted+gated; setuid labeled EMULATED not SUPPORTED; no unqualified "secrets never leak" phrasing; Option E row matches isolation.json. Clean apart from #1/#3/#9.
6. **Housekeeping**: every spike file introduced exactly once by 2138549, never edited after; ADR at HEAD == committed; tool sha256 in all 9 JSON match live binaries; no personal paths, no credentials. Exception: mid-review drift (#10).
