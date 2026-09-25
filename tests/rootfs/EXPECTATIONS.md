# T07 rootfs fixture expectations (C01–C04)

These are the independently auditable semantics the in-crate tests
(`crates/ls-host/src/{images/unpack,rootfs/prepare,rootfs/lease}.rs`)
assert against REAL artifacts. Fixtures are produced by the hand-rolled
ustar writer in `crates/ls-host/src/rootfs/mod.rs::testkit` — that writer is
FIXTURE INPUT only; extraction/whiteout/hardlink semantics are always
executed by the pinned `umoci` (docs/v5/09_RUNTIME_DETAILS.md §17), never by
LightSandbox code.

## C01 — malicious layers (unpack)

Two single-layer layouts:

| name    | layer entries                                                        |
|---------|---------------------------------------------------------------------|
| dotdot  | `../evil-t07` file                                                  |
| abslink | symlink `s -> ../../../../etc`, then file `s/evil-t07` through it    |

**Measured behavior of the pinned umoci on this host (2026-09-24 probe,
`/tmp` scratch, both cases exit 0):** umoci CONFINE-SANITIZES rather than
rejects — `../evil-t07` is rewritten to `rootfs/evil-t07`, and the write
through the escaping symlink is evaluated under the rootfs
(`rootfs/etc/evil-t07`); the symlink itself keeps its verbatim target (a
symlink pointing outside is legal in an image and is confined by the
runtime mount namespace). No host-side artifact was produced.

The asserted contract invariant is therefore **confinement**, in BOTH
outcome arms (a future umoci upgrade may reject instead — that arm is also
accepted and asserted):

* reject arm: structured `ImageError` (`IMAGE_UNPACK_FAILED` or
  `IMAGE_LAYOUT_INVALID`), no partial destination, no escape artifact;
* sanitize arm: containment audit passes over the produced tree AND no
  file named `evil-t07` exists anywhere in the case dir outside the
  bundle.

The mechanism observed (`REJECT` vs `SANITIZE-CONFINE`) is printed per
case for the record. The post-unpack containment walk (non-symlink-
following canonicalize check) remains the second-line audit.

## C02 — layer semantics tree (both drivers identical)

Fixture layers (`fixture:latest`):

```text
L1:  ro.d          dir      0555
     data.txt      file     "base\n" 0644
     hard          hardlink -> data.txt
     link          symlink  -> /etc/hostname
     keep.txt      file     "k\n"    0644
L2:  data.txt      file     "v2\n"   0600
L3:  .wh.link      whiteout
```

Expected final rootfs (asserted via a full-tree snapshot walk, content +
mode + kind + verbatim symlink target strings):

| path     | expectation                                                        |
|----------|--------------------------------------------------------------------|
| data.txt | content `v2\n`, mode 0600 (override REPLACES content and mode)     |
| hard     | content `base\n`; MUST NOT share an inode with data.txt (override gives data.txt a NEW inode; the L1 pair keeps the old one, per Docker/OCI apply semantics) |
| link     | ABSENT (whiteout removal)                                           |
| keep.txt | content `k\n`, mode 0644 unchanged                                  |
| ro.d     | kind dir, mode 0555 (directory mode preservation)                   |

If pinned `umoci` is absent on a host the test skips with an explicit
`SKIP c02` stderr line (absence is a host fact, not a pass). If umoci's
hardlink-vs-override behavior ever differs from the table, that is a real
semantics finding to record — the fixture must not be edited to match.

## C03 — per-sandbox isolation + honest sharing

One layout, two sandboxes prepared back-to-back (a=FULL_COPY,
b=REFLINK_COPY) from the same per-digest verified base:

* both trees satisfy the C02 table;
* writing `keep.txt` = `A\n` in a and `B\n` in b each sees only its own
  write;
* the shared base at `bases/<manifest-hex>/rootfs/keep.txt` remains `k\n`
  (base immutability — the base is the sharing source for reflink and must
  never be written after publication);
* both drivers report `logical_bytes > 0` and their driver name.

### C03-cost — padded tree (64 MiB pad)

RED history (all real, in order): (1) tiny tree (10 bytes logical) →
df-delta signal drowned in fs block granularity (~24 KiB); (2) 4 MiB pad
→ negative deltas (-57 KiB/-20 KiB) from concurrent allocation/free on the
shared 7.5 TB dev volume; (3) 64 MiB pad STILL showed ~zero FULL_COPY
allocation with a 21 ms copy wall — root cause MEASURED: `std::fs::copy`
uses `copy_file_range(2)` which performs clone-offload on this host's
overlayfs, silently turning the "full copy" into a reflink sharing extents
with the shared per-digest base (isolation still held via COW; the COST
contract was being faked). Fix: FULL_COPY copies via an explicit
userspace 256 KiB read/write loop (`copy_file_real`) with `sync_data`
before the measurement window — real allocation by construction, and the
64 MiB signal sits two orders of magnitude above the measured noise floor.
du/st_blocks are NOT usable for reflinks (logical blocks);
`estimated_shared_bytes` is clamped to `[0, logical_bytes]`.

Ambient-load honesty: the dev volume is shared; df deltas can be perturbed
by unrelated tenants in either direction. Tolerances below absorb routine
noise; a violation prints the raw numbers. If CI ever flakes here, the fix
is isolated CI storage — not a looser assertion.

* FULL_COPY: `allocated_delta_bytes >= logical_bytes / 2` (it really
  allocates; concurrent frees on the shared fs can only shrink the
  observed delta, never fake it upward — a full copy cannot lose blocks);
* REFLINK_COPY: `allocated_delta_bytes <= logical_bytes / 8` (metadata +
  dir blocks only — extents shared with the per-digest base), strictly
  less than the FULL_COPY delta, and `estimated_shared_bytes >=
  logical_bytes / 2`;
* the 64 MiB payload round-trips byte-exact under both drivers;
* df unmeasurable on a host → the cost assertions degrade to a printed
  NOTE (never a fabricated number, and the NOTE is not counted as a pass
  of the cost property).

Negative contract: ReflinkCopy requested where FICLONE probing fails must
return `HOST_UNSUPPORTED`; there is no silent fallback to FULL_COPY.

## C04 — lease lifecycle and GC

* prepare marks `.incomplete` first; commit writes `lease.json` and clears
  the marker; `release()` removes the sandbox dir and re-sweep finds no
  debris;
* `sweep()` removes ONLY `sandbox-*` dirs carrying `.incomplete`, not in
  `active_ids`, with marker age >= cutoff, and no live mount reference;
  committed dirs, `bases/`, and non-`sandbox-*` entries are never touched;
* `Duration::ZERO` cutoff = force-clean (matches any age); production
  callers must pass a nonzero cutoff so in-flight prepares are protected;
* mount-leak audit: with bind-mount privilege (uid 0 here) a real leaked
  bind mount under a stale sandbox dir makes the sweep REFUSE deletion and
  enumerate the dir in `refused_mount_leak`; after umount the sweep removes
  it. Without mount privilege the test skips with an explicit line;
* `Drop` on `RootfsLease` performs NO disk mutation: unreleased leases are
  reconciled by sweep/active-ids, never by an implicit destructor delete.
