# T09 resources / admission expectations (C01–C05)

Independently auditable semantics for hard limits, node admission and
metrics honesty. Three layers like every gate in this repository:

1. **Laws (always on, no host dependency):** 10 `ls-core::admission` unit
   tests + 8 `ls-host::resources` unit tests prove the arithmetic and the
   orchestration laws on fixture trees — no fixture ever impersonates a
   MOUNT (root bypasses chmod bits, so read-only-ness is injected via the
   mounts table, exactly like the kernel reports it).
2. **REAL host tier (`LSB_T09_REAL=1`):** 6 integration tests in
   `crates/ls-host/tests/resources.rs` against the real `/proc`,
   `/proc/mounts` and `/sys/fs/cgroup`.
3. **BLOCKED, criterion unchanged:** anything needing cgroup WRITES
   (delegated subtree).

## Host capability measured (2026-09-26, this host class)

```
mount: cgroup /sys/fs/cgroup cgroup2 ro,nosuid,nodev,noexec,relatime 0 0
controllers:        cpuset cpu io memory hugetlb pids rdma misc
subtree_control:    (empty)
mkdir probe:        euid-0 → EROFS (os error 30); su nobody → EROFS
writable subtrees:  none anywhere (find -writable: empty)
delegation (65534): none
/tmp (sandbox scratch): overlayfs on / — no prjquota/usrquota/grpquota
cpu.max (root):     "2000000 100000"   memory.max (root): "85899345920"
```

Consequence per criterion:

| ID | Criterion | Verdict on this host |
|----|-----------|----------------------|
| T09-C01 | hard mem/PID enforcement + sibling isolation | **BLOCKED** — no writable delegated subtree; SKIP-with-evidence, criterion untouched |
| T09-C02 | required storage hard limit on quota-less fs ⇒ reject BEFORE create | **REAL PASS** — the premise (no quota) is this host's truth |
| T09-C03 | read effective cpu quota/period; unit conversion exact | **REAL PASS** (readback tier) |
| T09-C04 | contention/cancel: no oversell, no leak, no starvation | **REAL PASS** (threaded) |
| T09-C05 | capacity ≠ limit ≠ usage; window recorded; no limit-as-usage | **REAL PASS** |

## Admission laws (ls-core::admission, 10 tests)

1. Fit-now admits; exhausted-but-possible queues; queue-full rejects with
   `QueueFull{capacity}` — backpressure, never an unbounded park.
2. `Impossible` requests (exceed the TOTAL budget) are rejected at the door
   with the dimension named — never parked into guaranteed starvation.
3. **Anti-starvation fairness:** while the queue is non-empty, EVERY new
   request queues behind it (a stream of small requests can never snatch
   released capacity ahead of a big head). Promotions are strict FIFO; a
   misfitting head blocks honestly — and it can only be a POSSIBLE request
   (law 2), so later releases will fit it.
4. Allocation/Ticket are consumed by release/cancel — double release is
   unexpressible (ownership law, not a runtime check). Cancelling a seq that
   was already promoted returns false (取消已接受不是取消已完成).
5. Conservation under contention: 8 threads × requests, mixed cancels —
   every non-cancelled request is materialized exactly once; drain ends
   pristine (held=0, remaining=budget, queued=0). Measured REAL tier:
   `admitted=8 promoted=92 cancelled_skipped=92` (192 requests total: 8
   admitted, 184 queued, 92 cancelled ⇒ exactly 100 materialized).

## Plan/probe/writer/metrics laws (ls-host::resources, 8 tests)

1. Unsupported controller + requested hard limit ⇒ the WHOLE plan rejects
   BEFORE create; **every** unsupportable field is named with bound evidence
   (没有某控制器时不能静默丢字段). Measured REAL: storage on `/` overlay
   without quota opts ⇒ `RejectReason{field:"storage_bytes", evidence:
   "covering mount / has no project/user/group quota option (opts: rw,
   relatime,lowerdir=…)"}`.
2. Unconstrained request ⇒ Enforce with `CgroupSpec::default()` — rejection
   is scoped to real demands (REAL-tier sibling test).
3. Writability is two-signal: mounts-table `ro` (authoritative — the kernel
   enforces it for euid-0 too) AND a real transient mkdir+rmdir probe
   (catches rw-but-not-delegated). Both are recorded verbatim.
4. cgroup writes verify by read-back (`cpu.max`/`memory.max`/`pids.max`);
   mismatch ⇒ `Verify` error with wrote≠read shown. Conversion
   `millis↔quotaµs` is checked arithmetic with exact roundtrip at the REAL
   period (100000µs measured).
5. Metrics honesty: `capacity` (real MemTotal: 1622731829248 B measured),
   `configured_limit` and sampled `usage` are distinct fields; sampling
   source is `VmHWM` (physical high-water — an ASAN virtual-address clamp is
   never physical billing). Unsampleable usage ⇒ `None` + `Unavailable`,
   NEVER the limit. Measured REAL: limit=Some(67108864) vs usage
   991232→991232 B under a live busy process; window_ms recorded verbatim
   (150); HWM monotone asserted.

## Gate protocol

```bash
# laws + fixtures (always)
cargo test -p ls-core admission && cargo test -p ls-host resources
# real host tier
LSB_T09_REAL=1 cargo test -p ls-host --test resources -- --nocapture
```

Denominator: with the gate armed, 6 tests; C01 currently prints the BLOCKED
evidence above and skips its enforcement arm — if this suite ever runs on a
delegated host, the SAME test executes the apply-and-verify arm with no code
change.

Privileged runner gates (`./dev/check runtime --task T09 --worker …`) need
an authorized worker; none exists in this package — recorded, not simulated.
