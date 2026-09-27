# T08 lifecycle vertical-slice expectations (C01–C04)

Independently auditable semantics asserted by
`crates/ls-host/tests/lifecycle.rs` against the REAL chain
`manager → journal → image-load-verify → rootfs lease → native backend →
rootless runc → lsguest-init`. The suite is two-tier, like every other gate in
this repository: 8 scripted-kernel tests run unconditionally; 3 REAL-runc
tests are gated (see below) and SKIP loudly when the gate is not armed.

Denominator honesty: with the gate armed the file reports **11 tests**: the
3 real ones run; without the gate they still count as passed-but-skipped
(each prints a `SKIP ...` reason to stderr, per repo convention).

## The scripted-kernel tier (8 tests, always on)

A `FakeBackend`/`FakeKernel` with injectable failure points proves the
ORCHESTRATION LAWS without a kernel:

1. `create_persists_record_with_legal_states` — create mints exactly one
   instance label (`lsbx-*`), one durable record with `state=running /
   lifetime=ACTIVE`, `revision >= 2` (acceptance + registration hops), and
   inspect is observation-driven.
2. `t08_c03_backend_create_failure_rolls_back_everything` — backend-create
   failure ⇒ strictly zero residue: no lease dir, no layout view, no record,
   no live container.
3. `t08_c03_start_failure_compensates_through_the_same_boundary` — the FAILED
   step registers no compensation: exactly the earlier backend-create is
   rolled back (LIFO), through the SAME backend boundary (cloned handle),
   never by manager-side path surgery (`deleted == 1` is the assert).
4. `execute_maps_termination_and_fences_generation` — Exited ⇒
   `exit_record=Exited{0}`, `termination=None`; a wrong generation ⇒
   StaleGeneration (no side effects).
5. `execute_timeout_never_fabricates_an_exit_code` — Timeout ⇒
   `exit_record=Unknown` + `termination=Timeout`; NEVER a fabricated code.
6. `delete_archives_receipt_retires_and_refuses_reentry` — delete archives the
   ORIGINAL receipt bytes, retires the record (`DELETED`), denies re-entry
   (NotFound), and returns `CleanupInfo` even on the unhappy path.
7. `degraded_delete_lands_in_cleanup_required_and_retry_converges` — a failing
   cleanup step ⇒ `CleanupRequired` (still ACTIVE), retry converges to
   `Deleted`; no step is ever skipped silently.
8. `t08_c04_reopen_reconciles_record_against_live_kernel` — reopen of the
   manager process reconciles every persisted record against the LIVE kernel
   enumeration; foreign (`lsbx-*` but unrecorded) instances are REPORTED, not
   adopted.

## The REAL rootless-runc tier (3 tests, gated)

Arm with `LSB_T08_REAL=1`. Prerequisites, all checked loudly at run time:

- pinned runc at `/opt/toolchains/lightsandbox/bin/runc` (1.5.0-rc.1);
- **statically linked** `target/x86_64-unknown-linux-musl/debug/lsguest-init` —
  build with `cargo build -p ls-guest --target x86_64-unknown-linux-musl`
  (the musl target is the canonical witness contract, ADR 0001 A4: true
  static-PIE, no loader dependency; supersedes the interim gnu crt-static
  path. A plain `cargo build` produces a DYNAMIC binary that cannot start
  inside an arbitrary user image — measured defect, see fixes below);
- a user image assembled by the test itself: `/bin/dash` + libc + ld-linux
  (hand-rolled POSIX ustar fixture, checksum field computed with the space
  convention; layout annotation `"runtime"`);
- **PORTABLE_ROOTLESS identity `(65534, 65534)` end to end** — euid-0 on this
  host class takes runc's ROOTFUL cgroupfs path and dies on the read-only
  `/sys/fs/cgroup` (measured; identical conclusion in `spike/lab.py`).

### C01 — `create → exec(read-back) → delete` is REALLY real

The created sandbox runs the image entrypoint (`dash -c 'while :; do :; done'`,
dash-only longevity — the image has no coreutils). `execute` spawns a real
`runc exec` whose argv writes a marker file and reads it back with PURE
BUILTINS (`echo`, `read`, `[`) — `cat` is absent from the image by
construction, which is exactly what caught the R4 fixture defect. Assertions:

- `state=Exited` with `exit_record=Exited{exit_code:0}` — the spec'd cmdline
  RAN with our arg semantics (no shell guessing, no fixed response);
- captured stdout equals `RBCHECK:T08_WRITE_OK` — real bytes crossed the real
  pipe; never a canned/global result;
- `execution_id` is a per-call minted id;
- delete returns `all_ok` — every cleanup step really ran — and leaves no
  live container and no lease residue.

### C02 — the entrypoint's exit code is the truth

Image entrypoint `dash -c 'exit 7'`. Readiness honesty: the post-start kernel
observation MAY already be `Stopped` (legal direct hop, frozen table). The
init receipt MUST say `Exited{exit_code:7}` — a real termination, never a
Running-forever success — and delete MUST archive receipt bytes containing
`"exit_code\":7`.

### C04 — reopen believes the kernel, never the database

Create with manager instance A; drop A (no daemon needed — the container
survives its manager). Open manager instance B as a fresh PROCESS over **the
same durable roots** — manager state, runc state root, native root. runc's
container registry is SCOPED BY ITS STATE ROOT: a fresh state root would make
the live container invisible (the R4 test-design defect). B's open report must
see exactly 1 record and 0 foreign instances; inspect MUST reconcile to the
kernel-observed `Running` (the record alone was never proof); a subsequent
exec MUST reach the STILL-RUNNING sandbox; delete converges.

## Product defects the real slice surfaced (and their fixes, 2026-09-25)

1. **lsguest-init MUST be statically linked.** First real boot:
   `/.lsbx/init: error while loading shared libraries: libgcc_s.so.1` — an
   arbitrary user image guarantees no glibc. Fixed first by crt-static build,
   now formalized as the musl target (ADR 0001 A4)
   (musl-target migration is a follow-up discussion, not a blocker).
2. **The backend must chown the `.lsbx/` DIRECTORY, not just the init file.**
   First real receipt: `receipt write failed: Permission denied` — the
   runtime-identity uid could not create `/.lsbx/receipt.json` inside a
   root-owned directory. Fixed in `runtime/native/mod.rs` (parent dir chown
   first).

## Measured evidence (2026-09-25, this host)

- `LSB_T08_REAL=1 cargo test -p ls-host --test lifecycle -- --test-threads=1`:
  **11 passed; 0 failed; 2.59 s.**
  - C01: `exec_id=exec-…-18d89b9dd666fe2b wall=20ms stdout="RBCHECK:T08_WRITE_OK" truncated=false`
  - C02: `receipt=InitReceipt{sandbox_id:"sbx-…", child_pid:Some(7), termination:Exited{exit_code:7}}`
  - C04: `reopen->exec OK wall=171ms`
- Workspace: 224/224 (ls-core 110, ls-host lib 84, ls-guest 12, ls-api 7,
  lifecycle integration 11). `./dev/check fast`: **325/325, exit 0**.
