# Round-04 Independent Review — UML Spike Evidence & ADR 0002

Fresh-context verification of the round-03 UML spike evidence package against the
claims in `docs/adr/0002-capability-adaptive-backends.md`. Read set (only):
ADR 0002; `spike/results/uml-boot.json`; `evidence/runtime/uml/boot-console.txt`;
`spike/uml/source-lock.json`; `progress/2026-09-25-round-03.md` (§Experiments +
§UML verdict follow-up only); `spike/results/uids.json`; `spike/results/p21.json`.
No tests run, no source modified; this file is the review's only write.
Line refs: ADR = docs/adr/0002-capability-adaptive-backends.md,
JSON = spike/results/uml-boot.json, LOG = evidence/runtime/uml/boot-console.txt,
LOCK = spike/uml/source-lock.json, PROG = progress/2026-09-25-round-03.md,
UIDS = spike/results/uids.json, P21 = spike/results/p21.json.

## VERDICT: PASS-WITH-MAJORS

The core claim — a single retained run achieving BOOT_OK with guest-owned /proc,
chown/su multi-UID evidence and a guest-DAC probe, on a CONTAINERIZED +
PORTABLE_ROOTLESS host, no sudo/KVM — is supported by LOG and JSON, which are
mutually consistent (JSON console_tail JSON:21-47 reproduces LOG:61-85
line-for-line; the five parser markers JSON:8-14 all appear as console lines;
`host rc=0` JSON:4 matches `reboot: Power down` LOG:84 plus orderly exit).
EXPERIMENTAL is the correctly-scoped verdict. Two MAJOR evidence-lineage
defects (final-run timestamp contradiction; artifact-less cleanup assertion in a
durable ADR) and seven MINOR record-quality items must be fixed before this
package is quoted downstream.

## Checks performed

- Marker-by-marker cross-check of ADR §12.1 (ADR:190-196) against LOG:70-84.
- JSON schema split (markers vs fields vs console_tail) vs the ADR's "markers all true".
- ADR §2 gap claims vs UIDS/P21 verdict fields, verbatim.
- LOCK disclosure completeness (mirror, cross-check, PGP, per-artifact digests).
- Console-quirk chain internal consistency; alternative-mechanism review.
- Host scoping / generalization scan of ADR and PROG prose.
- Boundary declared: T09-C01 hard-cgroup artifact and run-1/2 logs
  (`spike/uml/work/…`, git-ignored per PROG:112) are outside the permitted read
  set; claims resting on them are marked unverified-within-scope, not contradicted.

## Findings

| ID | Severity | Artifact:line | Claim questioned | Verbatim evidence |
|---|---|---|---|---|
| F-01 | MAJOR | PROG:132 vs JSON:3, LOG:9 | Final BOOT_OK run pinned to 05:13Z in narrative | PROG:132 "**Final run `05:13Z`: `verdict=BOOT_OK`** — all five parser markers true" vs JSON:3 `"utc": "2026-09-25T05:46:53.554042+00:00"` and LOG:9 "#8 Fri Sep 25 05:46:46 UTC 2026". The retained package is self-consistent at 05:46Z (kernel build 05:46:46, run 05:46:53, guest file `Sep 25 05:46` LOG:79); 05:13Z — and PROG:112's "current on-disk JSON stamped 05:06Z" — cannot be reconciled with the artifact without hypothesis. Verdict substance unaffected; audit trail identity wrong. |
| F-02 | MAJOR | ADR:206-208 | Cleanup asserted as observed fact in a durable ADR with no retained artifact | ADR:206-208 "Cleanup: orderly shutdown removes `~/.uml/<id>/`; the one observed residue came from an intentionally-panicked diagnostic run and was reaped manually". LOG proves only dir creation (LOG:56 "mconsole (version 2) initialized on /root/.uml/9aNZc1/mconsole") and orderly power-down (LOG:84); no before/after listing exists. Panic reaping is narrative-only (PROG:140 "(`/usr/bin/rm -rf ~/.uml/onn9JQ`)"). Claim must be downgraded to "observed once, unrecorded" or backed by an artifact. |
| F-03 | MINOR | JSON:8-20,:42 vs ADR:191 | "markers all true" conflates parser markers with fields/tail | JSON markers map holds exactly 5 keys (`LSB_UML_BOOT_OK`, `LSB_CHOWN_OK`, `LSB_SU_OK`, `LSB_USER_WRITE_OK`, `LSB_UML_EXPERIMENT_DONE`, JSON:9-13); `LSB_PROC_OWN`/`LSB_HOSTNAME`/`LSB_STAT`/`LSB_CGROUPS` live in `fields` (JSON:16-19) and `LSB_DAC_ISOLATION_OK` appears only in `console_tail` (JSON:42). ADR:191 "Guest evidence (`uml-boot.json`, verdict `BOOT_OK`, markers all true)" therefore counts DAC as gated evidence although the documented gate is "verdict derives only from guest-emitted markers in kernel console output" (JSON:48) and DAC is not a JSON marker at all. |
| F-04 | MINOR | ADR:197-205; PROG:128-131 | "both proven" / "Root cause proven" overstate mechanism isolation | ADR:197 "Console plumbing root causes (both instrumented, both proven)". The console-drop mechanism is inferred from marker absence plus LOG:71 `LSB_CONSOLE_PROBE=open-ok` — the probe tests open via redirect, never a write-then-observe; "writes routed to an uncaptured console target" is not excluded. The devkmsg cause is evidence-consistent (LOG:70 `LSB_DEVKMSG_MODE_BEFORE=ratelimit`) but the fix bundled "init now mounts proc FIRST, records `LSB_DEVKMSG_MODE_BEFORE=ratelimit`, writes `on`, THEN sets up the channel" (PROG:130-131) with no single-change rerun recorded. The sleep-drain hypothesis WAS falsified cleanly (PROG:125-127, kernel #6) — that part of "proven" holds. |
| F-05 | MINOR | LOG:81; ADR:195-196 | "guest DAC enforced" rests on a bare marker | ADR:195-196 "`LSB_DAC_ISOLATION_OK` (uid 999 refused a uid-1000 0600 file — guest DAC enforced)". LOG:81 contains only the marker token; unlike chown (LOG:76 `LSB_STAT=1000:999 644`), su (LOG:77 `uid=1000(user) gid=1000(user) groups=1000(user)`) and the write probe (LOG:79 `-rw-r--r--    1 user     user             0 Sep 25 05:46 /workspace/by-user`), the refused-read transcript is not retained. Claim rests on trusting the init script's gating logic, which is outside the evidence set. |
| F-06 | MINOR | LOG:82; ADR:195 | `LSB_CGROUPS=6` semantics undefined | LOG:82 `LSB_CGROUPS=6` — no artifact states what was counted (cgroup mounts? `/proc/cgroups` lines? controllers?). It evidences "something countable exists" only; no guest cgroup functionality or limit enforcement is demonstrated. Host-side hard limits remain blocker-blocked (PROG:25-27). |
| F-07 | MINOR | LOCK:15-20,:21-46; LOG:9; ADR:187-189 | Lock completeness | m4/bison/flex/bc SOURCE hashes recorded (LOCK:31-44), but: (a) the bc-applet SHIM artifact that actually ran on PATH (PROG:101 "busybox bc applet shim verified byte-correct", PROG:172) has no digest and no retained byte-comparison output; (b) the built busybox binary inside the initramfs has no digest — only the source tarball (LOCK:27-29) — contrast with per-run `"kernel_binary_sha256"` (JSON:6); (c) host toolchain ambient/unlocked (LOG:9 "gcc (Ubuntu 11.4.0-1ubuntu1~22.04.3) 11.4.0, GNU ld (GNU Binutils for Ubuntu) 2.38"); (d) the kernel .config delta ("defconfig + DEVTMPFS/MAGIC_SYSRQ", ADR:187-188) is not captured in the lock. Acceptable for spike; not for reproducibility claims. |
| F-08 | MINOR | PROG:107-112 | Run-1/Run-2 evidence not retained | Run 1 (BLOCKED verdict), Run 2 (panic, host rc=134) live only in git-ignored `spike/uml/work/…` (PROG:112). The flush-race falsification on kernel #6 (PROG:125-127) and the "~9–12 user lines in every run" truncation claim (PROG:129-130) are therefore narrative-only. Final-run package is intact, so impact is limited to provenance depth. |
| F-09 | MINOR | ADR:181 vs ADR:52 | ACCEPTED gate list vs advertised Kernel workloads | ADR:181 gates ACCEPTED on "OCI materialization fidelity, base+COW, exec/control transport, clean-cleanup confirmation, full perf table" — it does not name systemd/init smoke, guest-networking stance, or guest-cgroup enforcement, while ADR:52 advertises the Kernel backend for "systemd, guest cgroup, iptables/kernel tasks". §4's measured-offers design (ADS:76-87) rescues coherence (ACCEPTED ≠ every §3 workload SUPPORTED), but the ADR never says so explicitly; readers can equate ACCEPTED with the §3 workload list. |

## A. Does BOOT_OK follow from boot-console.txt?

Yes. Every marker ADR §12.1 (ADR:190-196) claims is present verbatim in LOG:

- `LSB_PROC_OWN` — LOG:73 `LSB_PROC_OWN=yes`
- `LSB_CHOWN_OK` — LOG:75 `LSB_CHOWN_OK`
- `LSB_STAT` — LOG:76 `LSB_STAT=1000:999 644`
- su user id output — LOG:77 `uid=1000(user) gid=1000(user) groups=1000(user)` (plus LOG:78 `LSB_SU_OK`)
- `LSB_DAC_ISOLATION_OK` — LOG:81 `LSB_DAC_ISOLATION_OK`
- `LSB_CGROUPS` — LOG:82 `LSB_CGROUPS=6`
- `LSB_UML_EXPERIMENT_DONE` — LOG:83 `LSB_UML_EXPERIMENT_DONE`
- power-down — LOG:84 `reboot: Power down`
- supporting: LOG:72 `LSB_UML_BOOT_OK`, LOG:74 `LSB_HOSTNAME=lsbx-uml`,
  LOG:80 `LSB_USER_WRITE_OK`; host rc=0 per JSON:4 `"host_process_exit_rc": 0`.
- PROG:132-133 claims "12 LSB_ lines in log" — verified: LOG:70,71,72,73,74,75,76,78,80,81,82,83 = exactly 12.

Claimed-but-absent flags: no ADR-claimed console marker is absent. Three
qualifications, none fatal: (1) `LSB_DAC_ISOLATION_OK` is a bare token with no
refusal transcript (F-05); (2) `LSB_CGROUPS=6` has undefined semantics (F-06);
(3) the JSON verdict gate (JSON:8-14, 5 parser markers) does not include DAC,
PROC_OWN, STAT or CGROUPS — those ride as `fields`/`console_tail`, so
ADR:191's "markers all true" is loose phrasing (F-03). The five gated markers
plus `LSB_DEVKMSG_MODE_BEFORE=ratelimit` (LOG:70) and
`LSB_CONSOLE_PROBE=open-ok` (LOG:71) all check out.

## B. Over-promotion audit

None found on the prohibited axes; EXPERIMENTAL is the right verdict.
ADR:181 limits the claim to "boots CONTAINERIZED+PORTABLE_ROOTLESS, no
sudo/KVM, own /proc, true multi-UID + guest DAC (`spike/results/uml-boot.json`
BOOT_OK)" and names the outstanding gates verbatim: "ACCEPTED still requires:
OCI materialization fidelity, base+COW, exec/control transport, clean-cleanup
confirmation, full perf table". Networking: never claimed (the `PF_INET`
registration lines LOG:38-61 are kernel boot noise; no network test exists).
OCI fidelity: not claimed (ADR:209-210 "Performance table: NOT yet measured";
LOG:67 "Failed to initialize ubd device 0 :Couldn't determine size of device's
file" shows boot was pure initramfs — base+COW unexercised). Cleanup-under-panic:
explicitly NOT claimed handled — ADR:207-208 "`KernelInstanceSupervisor` must
reap `~/.uml/<id>` on abnormal exit". Security: ADR:212-220 (§13) states "A
guest kernel does NOT make UML a strong-security VM" and defers hardening to
"future work (T49)". PROG closeout agrees: PROG:20 "BOOT / PROC / MULTI-UID /
DAC GATES PASSED" paired with PROG:22-27 "UML acceptance: PARTIAL (missing at
close: OCI materialization fidelity, base + COW, guest control transport,
performance table …)". ACCEPTED would over-claim; PENDING SPIKE would
under-claim retained evidence. Sole overstated word: "proven" (F-04).

## C. Source-lock

Adequate for a spike lock, with honest disclosures and four gaps (F-07).

- Kernel + busybox digests: YES. Kernel 6.19.14 (LOCK:7-10), busybox 1.37.0
  (LOCK:11-14); hashes LOCK:23-24 `"sha256": "cde8bf6739be4a0777fedbbba5330b8188c55680c45a922a4dfa289cbec6f185", "file": "linux.tar.xz"` and LOCK:27-29 `"sha256": "3311dff32e746499f4df0d5df04d7eb396382d7e108bb9250e7b519b837043a4", "file": "busybox.tar.bz2"`.
- Mirror cross-check: YES, disclosed. LOCK:5 "kernel.org/busybox.net unreachable
  (TCP timeout) on this host 2026-09-25 ~03:30Z; all sources via the TUNA
  mirror"; LOCK:6 "sha256 cross-checked against the mirror copy of the
  upstream-published checksum files where available".
- PGP not verified: DISCLOSED. LOCK:6 "PGP signatures NOT verified (no keyring
  on host). Self-recorded otherwise." (Also PROG:171 decision 4.)
- busybox-binary + bc-shim provenance: PARTIAL. Source tarballs locked; the
  built busybox binary and the `$TOOLS/bin/bc` shim artifact are NOT digested;
  the claimed byte-correctness check (PROG:101, PROG:172) has no retained
  comparison output. Per-run kernel binary sha IS recorded (JSON:6).
- Tool used but unlocked: host gcc/binutils (LOG:9) — ambient host toolchain;
  m4/bison/flex/bc sources are locked (LOCK:31-44). Kernel .config delta not
  captured (ADR:187-188 names it only in prose).

## D. Console quirk chain

Internally consistent; mechanism isolation incomplete (F-04). Chain as
retained: kernel prints "Warning: unable to open an initial console." (LOG:68)
under cmdline "con0=fd:0,fd:1 root=98:0 console=tty0" (LOG:17); guest probe
reports LOG:71 `LSB_CONSOLE_PROBE=open-ok` — open succeeds; all markers arrive
over /dev/kmsg after LOG:70 `LSB_DEVKMSG_MODE_BEFORE=ratelimit` and an `on`
write (PROG:128-131). This is consistent with ADR:197-205's two causes:
(a) "/dev/console opens but silently drops userspace writes … (kernel-side
`console_on_rootfs` fails likewise)" and (b) "`kernel.printk_devkmsg=ratelimit`
… silently throttled userspace kmsg writes after the initial burst".
Alternatives not excluded: (1) console writes routed to an uncaptured target
(other than dropped) — the probe tested open, never write-then-observe, and no
captured run shows a /dev/console write attempted in the final configuration;
(2) the devkmsg fix shipped bundled with "mounts proc FIRST" (PROG:130) — no
single-change A/B rerun is recorded, so proc-order as a co-factor is not
eliminated; (3) the truncation quantification "~9–12 user lines in every run"
(PROG:129-130) is unverifiable — earlier logs git-ignored (F-08). The
falsification half is clean: sleep-1 shipped in kernel #6 and markers were
"STILL lost" (PROG:125-127), which genuinely kills the drain-race hypothesis.
Correct wording: "demonstrated consistent with both causes; drain-race
falsified; at least one routing alternative not excluded" — not "both proven".

## E. Cleanup

Requirement recorded: YES — twice. ADR:207-208 "`KernelInstanceSupervisor`
must reap `~/.uml/<id>` on abnormal exit" and PROG:140-141 "Supervisor
requirement recorded: KernelInstanceSupervisor reaps `~/.uml/<id>` on abnormal
exit."
Evidence for the factual claim "~/.uml/<id> removed on orderly shutdown": NOT
RETAINED (F-02). LOG shows only creation (LOG:56 mconsole socket under
`/root/.uml/9aNZc1/`) and orderly shutdown (LOG:84). "Panicked run residue
manually reaped": narrative with an exact command, PROG:139-140 "the single
residue dir traced to the intentionally-panicked diagnostic run and was reaped
(`/usr/bin/rm -rf ~/.uml/onn9JQ`)"; the panicked run itself corresponds to Run
2 (PROG:108, "Kernel panic - not syncing: Attempted to kill init!", host
rc=134), whose log is git-ignored. Verdict on E: supervisor requirement
properly logged; the cleanup facts are single-observation narrative and must be
re-asserted in round-04 only as "observed once, unrecorded".

## F. ADR structural

Coherent. Status ADR:3 "Status: **Proposed** (2026-09-25, Round-03); verdicts
in §12 are updated by the round-03 UML/QEMU spike evidence before this ADR is
closed" explicitly anticipates live §12 verdicts, so Proposed + filled verdicts
is internally consistent. §2 gap claims vs the cited artifacts:

- multi-UID BROKEN (ADR:37, "`uids.json`: BROKEN") — SUPPORTED. UIDS:202
  `"chown-to-1000": "BROKEN (exit=1)"`, UIDS:203 `"su-app": "BROKEN (exit=1)"`,
  UIDS:205 `"image-USER-1000": "BROKEN (exit=1)"`; top-level UIDS:209
  `"verdict": "single-ID multi-user semantics classified (tests/harness policy: no flatten-and-claim)"`.
  Nuance preserved by the ADR: UIDS also records UIDS:204 `"setuid-binary":
  "EMULATED (exit=0)"` and five SUPPORTED rows — the table row (ADR:37) fairly
  characterizes the gap without flattening.
- private /proc CONTENT_VISIBLE (ADR:38, "`p21.json`") — SUPPORTED and
  precisely scoped. P21:487 `"proc_confidentiality": "CONTENT_LEAKS_CROSS_SANDBOX"`;
  marker-visible paths limited to cmdline: P21:481-483
  `"cross_sandbox_marker_content_visible_paths": ["cmdline"]`; verdict P21:495
  `"P2.1: cross-sandbox CONTENT_LEAKS_CROSS_SANDBOX; control leaks=['cmdline']"`.
  ADR:38's "argv CONTENT_VISIBLE cross-sandbox" matches exactly — environ/cwd
  were ACCESS_DENIED (P21:103-117) and the ADR does not claim otherwise.
- no hard limits (ADR:40 "no writable delegation") — the T09-C01 artifact is
  outside my permitted read set. Corroborated within scope only by narrative:
  PROG:25-27 "T09-C01 hard cgroup enforcement stays blocker-blocked on the
  read-only host cgroupfs" and PROG:91 "C01 blocked-with-evidence (read-only
  cgroupfs, zero delegation)". Unverified-within-scope, not contradicted.
- systemd (ADR:39) and iptables/guest modules (ADR:41) are labelled
  "structural" — no artifact owed; wording is honest.

## G. Missing-for-production (before UML is production-credible)

Status against the required checklist:

| Item | State | Evidence |
|---|---|---|
| boots | ✓ spike-level, one retained run | JSON:7 `"verdict": "BOOT_OK"`; LOG:72 |
| private /proc | ✓ spike-level | LOG:73 `LSB_PROC_OWN=yes` |
| multi-UID | ✓ probe-level (chown/su/write; DAC bare probe) | LOG:75-80 |
| OCI fidelity | ✗ none — pure initramfs boot | LOG:67 ubd failure expected; ADR:181 gate |
| base+COW | ✗ never exercised | ADR:181 gate; PROG:24 |
| guest-control exec | ✗ console tty writes disproven; `ssl0`/mconsole only named | ADR:200-203 "a to-be-validated channel (serial `ssl0` or mconsole)" |
| exit semantics | ✗ no guest exec/wait-receipt protocol exists for UML | no marker/artifact |
| orderly cleanup | ~ claimed, unrecorded | F-02 |
| abnormal cleanup | ✗ manual once; supervisor req only recorded | PROG:138-141 |
| restart story | ✗ no lost-execution/reconcile design for UML | — |
| perf table | ✗ admitted | ADR:209-210 "Performance table: NOT yet measured (boot latency, exec latency, fork-heavy, COW create/delete, RSS)" |
| security boundary | ~ honest §13; hardening is "future work (T49)" | ADR:212-220 |
| systemd smoke | ✗ only busybox `/init` demonstrated | PROG:105 |

Formalize before production-credible: (1) OCI→UBD materialization fidelity
harness with ownership/xattr/whiteout parity vs the native unpacker;
(2) base+COW create/delete/GC semantics; (3) validated guest-control transport
(ssl0/mconsole) with the console-write quirk as a design constraint; (4) UML
exec + exit-receipt semantics equivalent to the T08/T10 native witness
(timeout→unknown law included); (5) recorded orderly-cleanup assertion
(before/after `~/.uml` listing); (6) fault-injected abnormal-cleanup tests
(kill -9, induced panic) under KernelInstanceSupervisor; (7) restart/reconcile
spec; (8) full perf table with denominators kept; (9) §13 hardening
implementation + residual-trust statement; (10) systemd/full-init smoke;
(11) an explicit networking stance — measured or declared unsupported;
(12) N-run boot stability (current: one retained successful run).

## H. Honesty

- Single-run generalization: mild and mostly well-scoped. The entire BOOT_OK
  package is ONE retained run (JSON:3; kernel build #8 LOG:9). ADR:181
  host-scopes correctly ("feasibility gate PASSED on this host") and ADR:103-106
  forbids generalizing this host's restrictions. But narrative habitually
  pluralizes ("boots fully", PROG:105; "GATES PASSED", PROG:20); round-04 must
  quote as "one retained run on this host".
- "Proven/passed" where artifacts say partial: two spots. ADR:197 "both
  instrumented, both proven" and PROG:128 "**Root cause proven**" — the
  artifacts support "consistent, one alternative not excluded" (F-04).
  Everything else tracks evidence honestly: PROG:94 kept "BOOT_PARTIAL (run 3:
  4/5 markers)" until the markers actually landed; PROG:113 "ADR 0002 UML
  verdict NOT flipped — stays PENDING SPIKE until `LSB_UML_EXPERIMENT_DONE`
  lands"; closeout PROG:22-27 records acceptance PARTIAL with named blockers;
  LOCK discloses PGP non-verification and mirror substitution. No fake success,
  no hidden denominator changes, no oracle edits in the read set.
- Record-level defects needing correction: F-01 (05:13Z↔05:46Z), F-02
  (cleanup fact), F-03 (markers-vs-fields phrasing in ADR:191).

## What round-04 must NOT claim from this evidence

Round-04 may say only: on this CONTAINERIZED+PORTABLE_ROOTLESS host, one
retained UML run (2026-09-25T05:46Z, kernel #8, sha256 477a39b5…) booted with
guest-owned /proc, demonstrated chown-to-arbitrary-uid/gid, a real `su` uid
switch, a uid-1000 write, and a guest-DAC refusal probe, then powered down with
host rc=0 — under a spike-level source lock with PGP-unverified mirror sources.
Round-04 must NOT claim: any networking capability; any guest cgroup
functionality or limits (`LSB_CGROUPS=6` is an uninterpreted count); guest-DAC
"proven" (bare marker, no retained refusal transcript, not a parser-gated
marker); OCI/UBD materialization, base+COW, exec/control transport, exit
semantics, systemd/init, or witness parity with the native T08/T10 slice — none
were exercised; cleanup "verified" (observed once, unrecorded; panic-residue
swept manually, supervisor requirement logged only); repeatability or stability
(n=1 retained run); any strong security boundary (ADR §13 is documentation with
T49 future work; upstream itself disclaims UML isolation strength); any
generalization beyond this host (ADR:103-106 forbids it); QEMU dispensability
beyond "not needed so far" (PROG:29-32); the §3 Kernel "target workloads" row
(ADR:52 — systemd/cgroup/iptables) as measured offers; or the 05:13Z run
timestamp, which contradicts the retained artifact and must be corrected to
05:46Z wherever propagated. The console evidence plumbing (/dev/kmsg +
printk_devkmsg=on) is proven-fit-for-spike-evidence only — it is NOT a validated
guest-control channel.

— End of review. Verifier: independent fresh-context round-04 pass. No other
file modified; no tests executed; verdict over the read set only.
