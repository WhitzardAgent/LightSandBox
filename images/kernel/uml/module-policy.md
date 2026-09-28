# UML kernel module policy (round-04 §48)

## Facts

- Spike kernel (phase-a → phase-f, sha256 `477a39b5012d…`): `CONFIG_MODULES=y`.
- The initramfs/disk distribution carries **no module tree** and no
  module-loading tooling. Any symbol built `=m` is therefore **dead weight**:
  `CONFIG_SLIP=m` in the current config can never load in any boot we have
  run. "m" in this product means "pretends to be available".
- ADR 0002 §13 hardening note already says: module loading disabled after
  boot if UML becomes a production backend.

## Policy (recorded decision)

1. **Production UML kernel: `CONFIG_MODULES=n`, everything needed is
   built-in (`=y`).** A guest that cannot load modules has a smaller attack
   surface and zero ambiguity between "configured" and "available".
2. Any feature requested as `=m` is a BUG against this policy: either
   promote to `=y` (with justification recorded in the config artifact) or
   drop the requirement.
3. The spike kernel keeps `CONFIG_MODULES=y` (unchanged, pinned) for
   continuity of evidence; the flip lands on `kernel-candidate-2` together
   with the required config flips in `README.md`.
4. Post-boot module loading in the guest init is REMOVED from the guest
   images' startup contract (nothing to load; nothing to disable twice).

## Rationale recheck before flip

When `kernel-candidate-2` is built, this policy is re-verified by booting it
and asserting: `/proc/modules` absent/empty, `init_module` syscall denied
(`finit_module` returns ENOSYS), and every REQUIRED symbol in the artifact
still functional (hostfs mount, UBD attach+COW, ext4, cgroup2).

## Candidate-2 implementation note (round-05)

- `CONFIG_MODULES=n` is now actually built: kernel-candidate-2 (binary
  sha256 `084b8c3983f6…03ec`, config sha256 `242fb1e03dba…f621d`) has
  **zero `=m` symbols** (`grep -c '=m$' .config` = 0). All 11 candidate-1
  `=m` symbols resolved: 10 promoted to `=y` (BFQ, BINFMT_MISC, LOOP, NBD,
  DUMMY, TUN, PPP, AUTOFS, ISO9660, SLHC) and the **CONFIG_SLIP dead module
  eliminated** (`# CONFIG_SLIP is not set`).
- Smoke boot passed (identical marker set vs candidate-1, no panic/BUG,
  host rc=0; `evidence/runtime/uml/phase-k2-smoke.txt`).
- The syscall-level recheck above (`/proc/modules`, `finit_module` ENOSYS,
  hostfs/UBD/ext4/cgroup2 still functional) is **pending**: the current
  embedded initramfs does not probe it; folded into the parent-tracked R3
  rerun on candidate-2. Keep candidate-1 pinned for phases A–F until it
  lands.
