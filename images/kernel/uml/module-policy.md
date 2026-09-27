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
