# Independent Review — T06 (OCI image import, a1590b6) + T07 (rootfs/reflink/lease, 5bf3792)

- Reviewer: fresh-context verifier agent (round-03, user instruction §2)
- Date: 2026-09-25 · Scope focus: correctness, second-backend (UML materializer)
  fitness of the image/rootfs abstraction, test honesty
- Verbatim verifier final report:

## Checks performed

- Full read of `crates/ls-host/src/images/{mod,layout,process_config,pull,store,unpack,error}.rs`, `crates/ls-host/src/rootfs/{mod,prepare,lease}.rs`, `tests/images/*`, `tests/rootfs/EXPECTATIONS.md`, `plans/tasks/T06.md`, `plans/tasks/T07.md` against the code.
- Executed `cargo test -p ls-host -- --nocapture`: **52 passed, 0 failed**. Evidence lines observed: `c01 dotdot/abslink: mechanism=SANITIZE-CONFINE (exit 0)`, `SKIP c04-mount: bind mount not permitted on this host`, `NOTE reflink-neg: host supports FICLONE …`, `HOST FACT: reflink_supported(tmp)=true`, c03-cost df-deltas (FULL allocated ≈ logical; REFLINK allocated = −331,776 → shared clamped to full logical).
- Reviewer-side experiment: reproduced piped-stdout + try_wait poll pattern with a 256 KiB-emitting child → child blocked on full pipe, parent would spuriously time out (confirms finding #1 mechanics).
- Verified pinned `/opt/toolchains/lightsandbox/bin/umoci` exists, sha256 matches recorded pin; host denies `mount(2)` despite uid 0; no `ls-host` types leak into `ls-core`/contracts.
- Cross-checked matrix cases m01–m13 against OCI/Docker combination semantics.

## Findings (severity-ranked)

**BLOCKER: no findings.**

1. **MAJOR — correctness — `pull.rs:271-303`.** Skopeo spawned with piped stdout/stderr while parent only polls `try_wait()` and never drains: >64 KiB output blocks forever → spurious timeout kills healthy child. Latent (no pinned skopeo → TransportUnavailable today), but sits on the credential path T06-C05 will enable. Fix: sidecar files (like `unpack.rs:120-136`) + `wait()` after kill.
2. **MAJOR — correctness/design — `lease.rs:214-264,92-99,36-44,124-126`.** Committed lease trees have NO reclamation path (sweep only covers `.incomplete`); `owner_pid`/`created_ms` written but never consumed; `commit()` not crash-atomic. Fix: committed-lease reconciliation class (stale lease.json + id∉active + owner-liveness), mountinfo refusal kept, lease.json via tmp+rename.
3. **MINOR — `pull.rs:141-146`.** Authfile written with umask-default mode (0644 window), chmod'd after. Fix: OpenOptions mode 0o600 create-new inside 0700 dir.
4. **MINOR — `prepare.rs:156-158`.** `prepare()` unconditionally remove_dir_all + swallows removal error before clobber-refusal. Fix: refuse when lease.json/mount refs exist; propagate removal failure.
5. **MINOR — `lease.rs:225,239-249,255`.** (a) one per-entry IO error aborts whole sweep; (b) marker mtime never refreshed vs long prepares; Fix: collect per-entry failures into report; document cutoff ≥ max prepare time.
6. **MINOR — `process_config.rs:101-108`.** Absent/null `.config` (valid OCI) rejected misleadingly. Fix: treat absent/null as empty object.
7. **MINOR — `prepare.rs:148-178+110-127`.** `prepare(layout_root, reference, image)` never binds layout to LoadedImage; `bases/<hex>` names digest A but umoci feeds `layout_root:reference` — mismatched pair silently wrong; store is not consumable OCI layout so unpack depends on caller keeping original layout alive. Fix: verify `layout_root:reference` resolves to `image.manifest_digest` in ensure_base.
8. **MINOR — test-honesty — `unpack.rs:415-425` + EXPECTATIONS.md:28-36.** Escape check lists only direct children; prose claims broader. Fix: recursive walk + explicit absence at /etc, tmpdir parent, CWD — or scope the prose.
9. **MINOR — test-honesty — EXPECTATIONS.md:132-135.** "(uid 0 here)" stale; on this host it printed `SKIP c04-mount: bind mount not permitted`. Fix: reword + capture mount-capable run later.
10. **MINOR — `prepare.rs:759-774`.** HOST_UNSUPPORTED-for-missing-FICLONE negative path unexecuted on this host. Fix: probe a genuinely non-FICLONE fs (e.g. tmpfs /dev/shm) or keep explicit skip.
11. **MINOR — `store.rs:181`.** Blob publish = atomic hard_link from caller layout: store aliases caller-mutable files; post-import edits silently corrupt store (no re-verify at consumption). Fix: import-time copy + chmod 0444, or re-verify at consumption.
12. **MINOR — `layout.rs:49-64,239`, `prepare.rs:178`.** `load_verified` slurps every layer fully into RAM per call; every create re-hashes all layers. Fix: streaming SHA-256; cache verification per digest behind a stated trust boundary.
13. **NOTE — `unpack.rs:84-96`.** Spike invoked `umoci unpack --rootless` explicitly; adapter omits the flag (uid auto-detect) → on root-in-container host exercised umoci's *root* mode, not PORTABLE_ROOTLESS target mode. Properties hold in both; pin flag for determinism.
14. **NOTE — c03-cost.** REFLINK delta −331,776 → clamped to full logical: honest estimate, not measured sharing.
15. **NOTE — `prepare.rs:253-269`.** `avail_bytes` spawns `df` via ambient PATH without env_clear discipline; PATH hijack could fabricate measurements (report-only, low blast radius).
16. **NOTE — misc.** (a) quarantine write failures silently dropped; (b) success-path umoci stderr sidecar discarded unread (sanitize warnings leave no record); (c) Platform variant rules stricter than OCI normalized matching + `Platform::host()` never populates variant; (d) env override replaces only first duplicate key; (e) ProcessSpec flattens null-vs-[] distinction (harmless for argv); (f) no crash sweeper for bases/staging/sidecars; (g) 3 cosmetic warnings.
17. **No findings:** mountinfo parsing (incl. octal unescape, prefix-boundary — unit-tested), hardlink-preserving full_copy + whiteout/override equality across drivers, atomic publish/idempotency/C04 race, reflink probe gating, Drop purity, env_clear+argv-only discipline (except df note), credential non-leakage, df-delta clamping rule — all hold. The umoci REJECT/SANITIZE dual-arm contract is defensible: confinement is the security invariant, both arms assert it, mechanism printed per case.

## Verdict

1. **Yes** — a UML materializer can be added without touching T06 code: `LoadedImage`/`LayerDescriptor`/`Platform` + config conversion are data-only and backend-neutral; `rootfs/`+`unpack.rs` are cleanly host-fs/Native-specific with zero leakage into ls-core/contracts.
2. **MUST change before production-true**: pull-path pipe-drain (#1), committed-lease GC gap (#2), layout↔image binding (#7); rest is MINOR hardening.
3. **Fine as-is**: digest-verification chain + platform gate, atomic store publish/quarantine/C04 race, config-conversion matrix, FULL_COPY/REFLINK honesty mechanics, umoci adapter discipline, sweep mount-leak refusal.

---

## Round-03 disposition (recorded by implementer)

Fix this round (review-follow-up commit): #1, #2, #3, #4, #5a, #6, #7, #8, #10 (explicit probe or skip), #12 (streaming hash), #13 (pin `--rootless` explicitly — deterministic regardless of euid), #15 (env_clear for df), #16a/b/g.
Documented-not-changed: #5b (cutoff ≥ max prepare time rule written into docs/comments), #9 (wording updated; mount-capable run still needed externally), #11 (copy-at-import implemented; hardlink fast path removed — cost paid once at import, correctness first), #14 (wording already honest).
