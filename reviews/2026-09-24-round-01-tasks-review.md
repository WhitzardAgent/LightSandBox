# REVIEW REPORT (round-01 tasks)

Reviewer: fresh-context verifier subagent (harness-level different-context review; NOT an independent organization security assessment)
Commits reviewed: `5b95372`, `b92736c`, `957fde7`, `95fadff`
Date: 2026-09-24
Scope file: docs/harness/03(规格/系统安全/测试三视角)。直接读取 diff、原始证据与源码。

## Per-commit verdict

| Commit | Verdict | Justification |
|---|---|---|
| `5b95372` design HostCapabilityTier | **CLEAN_WITH_NOTES** | Docs-only; internally coherent and later code matches. Minor: `03_DOMAIN.md` says "HostReport 不给 host 打单一标签" yet the T03 report/profile do carry a single `derived_tier` label; D01 `decisions.json` cites evidence files that only exist after T03 (same-day follow-through, verified present now). |
| `b92736c` T00 dev-entry | **CLEAN_WITH_NOTES** | Runner contract verified live: unknown suite rc=2 with suite list, `selftest-exit17` rc=1 with raw child code 17 preserved, `runtime` without worker rc=3 BLOCKED, `structure` rc=0, `fast` rc=0 tests_total=97. All subprocess argv-only, zero shell composition. Notes: card 修改范围 exceeded (`.gitignore`, `Cargo.lock`, `python/src`, `python/tests`, `plans/active/`, two empty `__init__.py`); claimed red-log `/tmp/t00-red-initial.log` is 0 bytes. |
| `957fde7` T01 source lock | **CLEAN_WITH_NOTES** | 11/11 LOCKED; tree hashes and file hashes independently recomputed for 4 repos and match; `--check-release` probed against 6 tamper classes, all rejected; dirty/missing-file/origin-mismatch paths probed, all reject. Notes: those `lock_one` rejection paths have no unit tests; release gate accepts any non-empty `role` and any `spdx` string; `dev/checklib.py`, `scripts/build-runtime-tools.sh`, `evidence/locks/` outside card 修改范围. |
| `95fadff` T03 HostReport | **MAJOR** | Evidence binding is real and verified (profile sha256 == committed bytes; statuses reproducible live). But three defects: (a) generator coerces an `UNSUPPORTED_HOST` tier into `PORTABLE_ROOTLESS` — a silent false label on hosts without userns; (b) `probe_network` creates a veth pair in the **caller's** netns despite the "read-only" probe contract (latent: `ip` is absent here); (c) `ls-core::derive_tier` keys on dotted finding names the shipped report does not emit, so the unit-tested Rust tier rules cannot actually consume the T03 report — cross-language "对齐" holds only at field shape. |

## Findings

**Blocker:** none.

1. **major** — `95fadff` — `python/src/lightsandbox_dev/host_probe.py:319`. `"derived_tier": tier if tier in ("PORTABLE_ROOTLESS", "FULL_ROOTLESS") else "PORTABLE_ROOTLESS"` — when userns is unavailable, `build_report()` computes `UNSUPPORTED_HOST` (line 305) and then silently rewrites it to `"PORTABLE_ROOTLESS"`. The validator (`capability_report.py:68-70`) only checks enum membership, and `test_derived_tier_is_portable_rootless` would still pass on the current host. On a host without userns support the tool would write a false admission tier. Evidence: source lines quoted above; `build_report()` branch at lines 303-305 can produce `UNSUPPORTED_HOST`, which is unreachable in output. Recommended fix: emit the computed tier (add `UNSUPPORTED_HOST` to the schema) or refuse to write/return non-zero when the computed tier is not a supported admission tier.

2. **major** — `95fadff` — `python/src/lightsandbox_dev/host_probe.py:191-194`. `probe_network` runs `["ip","link","add","lsbxprobe0","type","veth","peer","name","lsbxprobe1"]` (and a delete) in the **current** network namespace, not inside `unshare -Urn`. Module docstring promises "read-only argv probes … namespace-local … that die with the namespace"; AGENTS.md forbids host mutation without an authorized worker. On this host it exits 127 (`ip` missing) so nothing happened; on a host with `ip` + CAP_NET_ADMIN it would create host-visible interfaces (residual if the `del` fails; T03-C04 class). Evidence: the committed report's own N/A exit_code 127 (`hostreport-v1.json:65-66`). Recommended fix: execute the veth experiment inside `unshare -Urn` or drop it and rely on helper-presence + netns-content checks.

3. **major** — `b92736c`/`95fadff` — `crates/ls-core/src/lib.rs:36-41,107-131` vs `evidence/host/hostreport-v1.json` / `host_probe.py:292-293`. Tier derivation in Rust requires findings named `userns.single_id_mapping`, `userns.multi_id_mapping`, `cgroup.delegation`, `procfs.fresh_mount`; the shipped Python report emits flat names (`userns`, `uid_mappings`, `cgroup`, `procfs`, …). Feeding hostreport-v1.json to `HostCapabilityReport::derive_tier` returns `TierEvidenceError` (all untested), so the conservative, unit-tested Rust rules are dead code for the actual evidence artifact, and the Python probe reimplements tier logic (with finding #1's bug) instead. Neither the Python validator nor the T03 tests check name compatibility against ls-core. Recommended fix: single shared name registry (or a mapping layer) plus a cross-language test that `derive_tier` consumes the committed report to `PortableRootless`.

4. **minor** — all three implementation commits — task-card 修改范围 exceeded without card updates. T00 card allows `Cargo.toml`, `crates/*`, `python/pyproject.toml`, `dev/`, `tests/harness/`, `.github/workflows/`; the commit also added `.gitignore`, `Cargo.lock`, `python/src/*`, `python/tests/*`, `plans/active/T00.md`, `tests/native/__init__.py`, `tests/pack/__init__.py`. T01 card allows only `specs/{upstream-lock,protocol-inventory,sdk-lock}.json`, `tools/lock_*`, `tests/contracts/`; the commit also touched `dev/checklib.py`, `scripts/build-runtime-tools.sh`, `evidence/locks/`, `plans/active/T01.md`. T03 card lists `crates/ls-host/src/capabilities/`, `tests/runtime/capabilities/`, `specs/host-profiles.json`; the card-specified directories were never created, and implementation instead landed in `python/src/lightsandbox_dev/host_probe.py`, `python/tests/`, `evidence/host/`. All deviations are transparently disclosed in `plans/active/*.md` (T03: "ls-host crate 目前仅最小只读探针"), so this is bookkeeping drift, not concealment. Recommended fix: amend card scope in the integration pass or record the relocation in the cards themselves.

5. **minor** — `957fde7` — missing negative tests around `tools/lock_local_sources.py`. The unit suite covers short-revision and partial-lock rejection only; I independently confirmed the untested paths work (see spot checks): dirty clone → rc=1 "dirty source", missing required file → rc=1, origin mismatch → rc=1, invalid file-hash sha → gate rc=1, missing license bytes → rc=1, empty role → rc=1. Untested code: required-file path-traversal guard (`tools/lock_local_sources.py:55`), UNLOCKED stripping of `tree_hash`/`license` in partial output (line 126), `--output` exclusive creation. Also the release gate has no role/spdx allowlist — any non-empty `role` or invented SPDX string passes (the test suite pins roles/SPDX via the catalog tests instead, which is a reasonable but weaker enforcement point). Recommended fix: port my `/tmp/lockneg2` probes into `tests/contracts`.

6. **minor** — `95fadff` — `host_probe.py:316` hardcodes `"nested_container_traits": "cap 掩码 0xa80425fb(Docker 默认集),cgroupfs ro,见 identity_observation"` into every regenerated report. The mask is from a prior exploratory observation, not re-measured; regenerating on another host would silently carry this host's traits. Also `03_DOMAIN.md` ("HostReport 不给 host 打单一标签") vs shipped `derived_tier` + profile binding is an unresolved doc/artifact tension. Recommended fix: re-read `/proc/self/status` CapEff/CapBnd live, and reconcile the doc sentence or the artifact.

7. **minor** — `b92736c` — the claim "原始红日志 /tmp/t00-red-initial.log" (`plans/active/T00.md`, commit message): the retained file exists but is **0 bytes** (`-rw-r--r-- 1 root root 0 Sep 24 07:31`), so T00's tests-first red evidence is not auditable in the artifact. T01/T03 red logs are genuine (KeyError `'license'`/`'tree_hash'`, 2F/6E). Recommended fix: none retroactively possible; note the gap; keep red logs under `dev/.check-runs/` or evidence/ going forward.

8. **minor** — `b92736c` — CI (`.github/workflows/ci.yml`) implements real gates for what it wires (pinned toolchain, `structure`, `unit`, `fast`) and honestly omits runtime suites. But nothing in CI runs `tools/lock_local_sources.py --check-release` or `packctl.py release-check`, so upstream-lock drift would not be CI-blocked; there is no protected-path review gate either. No card required this for round 01, and `docs/harness/04:32` assigns CI/signing/branch protection to the project owner — flagging as a deployment-level gap to close before the T41 gate, not a defect in these commits.

9. **minor** — `b92736c` — `dev/checklib.py:438-440`: any `--flag` followed by another token swallows that token as its value, even if it is another flag or a would-be positional (`dev/check unit --task fast` silently runs `unit` with `task='fast'`). Low impact (options only gate worker/benchmark suites), but the parser is hand-rolled where `argparse` was available.

10. **informational** — inter-probe drift: 09-23 exploratory evidence records `max_user_namespaces=6190011`; the 09-24 formal report records that path as absent. Live check now: `cat: /proc/sys/kernel/max_user_namespaces: No such file or directory` — so T03's record matches the host's current state and the older file is stale session state (nested-container /proc masking differs between sessions). No action; evidence files could cross-reference this explicitly. Separately: the shared worktree received **concurrent writes by another process** during my review (`spike/lab.py` modified 14:08, untracked `spike/results/p21.json`, `evidence/security/proc-confidentiality.json`) — my verified `fast` run reports `"dirty": true` for that reason; reviewers should not treat a dirty flag as runner misbehavior here.

## Spot-check evidence

Lock tree hashes (lock values from `specs/upstream-lock.json`):
```
$ git -C /opt/src/runc rev-parse HEAD ; rev-parse HEAD^{tree}
41b74772b651b3b42a1f04a43a803db16f0e7e9b   == lock revision ✓
630e8a7537f207e3524f9ee35f88cc0fb5eee002   == lock tree_hash ✓
$ git -C /opt/src/umoci rev-parse HEAD ; HEAD^{tree}
f5d1219acaf67127ebacf6306776d3ff465735ea   ✓ ; 19fe2786290b3f1022291c726208552ad1cf03f9 ✓
$ git -C /opt/src/image-spec: ca68a05fad73… ✓ ; 9f134743f83d66d9bd7072312ab15bb010b2efd9 ✓
$ git -C /opt/src/proot:   58c3b42849fa… ✓ ; 7a019d24dbd0fd7058619970dbd287e689b890c1 ✓
(working trees clean: `git status --porcelain` empty for all four)
```
Lock file-content hashes:
```
git -C /opt/src/runc show 41b74…:README.md      | sha256sum → 90c9050f…2d95  == lock ✓
git -C /opt/src/runc show 41b74…:docs/terminals.md → 4d267bb6…4997 == lock ✓
git -C /opt/src/proot show 58c3…:COPYING → 0ffa6454…228a6d == lock ✓ (GPLv2 text confirmed)
git -C /opt/src/umoci show f5d1…:COPYING → cfc7749b…0ab4 == lock ✓
```
Release-gate independent tamper probes (all rc=1, offender named): `revision='main'` → "not a complete immutable commit"; top status PARTIAL → rejected; deleted `tree_hash` → "missing tree hash"; file-hash sha256='zz' → "invalid sha256"; license MISSING → "license bytes not recorded"; role='' → "role not recorded". Real catalog: `python3 tools/lock_local_sources.py --check-release` → rc=0 "all registered upstream sources are immutably locked".

dev/check live behavior (`/usr/bin/python3.12`):
```
dev/check fast --json        → exit 0, tests_total=97, failures=0, blocked=0
   breakdown: cargo 18 / harness-unit 5 / python-dev 16 / harness-all 14 / pack 34 / contract-locks 10
dev/check structure          → exit 0, 0 findings
dev/check selftest-exit17    → rc=1, child exit=17 preserved
dev/check bogus-suite        → rc=2, "unknown suite … available: unit, fast, structure, …"
dev/check runtime --task T00 → rc=3 "requires --worker WORKER_ID of an authorized worker"
```
HostReport binding: `sha256sum evidence/host/hostreport-v1.json` = `241e38420df2…520d4` == `specs/host-profiles.json` `report_sha256` ✓. Report statuses match the 09-23 transcript (`rootless-host-report.md/.json`) for cgroup EROFS, empty subuid, missing helpers, /dev/fuse absent, procfs EPERM, netns lo-only, disk-overlay EINVAL, AppArmor Y-unreadable; `max_user_namespaces` discrepancy resolved in T03's favor by live state (path absent now).

## Verified-clean checks

- No `shell=True`, `os.system`, string-formatted subprocess, or glob-into-argv anywhere in `dev/check`, `dev/checklib.py`, `tools/lock_local_sources.py`, `python/src/`, `tests/harness/`, `tests/contracts/` (grep-verified); lock tool invokes git with argv + hook suppression (`core.hooksPath=/dev/null`, `GIT_TERMINAL_PROMPT=0`, 30s timeout).
- No secrets/token-like strings in the full diff `bc69c5c..95fadff` (token/secret/key/Bearer/PEM sweep, zero hits).
- No factual "97 tests" inflation: reproduced rc=0/97 with identical per-collector breakdown; historical claims are consistent (30→39 unit, 78→88→97 fast).
- Structure lint negative space real: unknown-crate, reverse `path`-dependency, forbidden vocabulary, `#[ignore]` all caught by tests on temp copies (and unknown crate dir rule covers new scanner backends as top-level crates); T01-C03-adjacent "workspace registry false positive" fix verified in code (`workspace_internal` path-only check).
- No false-pass mechanics: zero-collected converts to failure in both `cargo_test` and `python_unittest` (incl. unittest rc=5), hard failures counted for any non-0/3/4 exit, blocked-only ⇒ rc=3, prereq ⇒ rc=4; `TEST_COLLECTING` guard catches empty suites at suite level.
- `test_host_probe.py` is not vacuous: re-runs `build_report()` twice, requires stored statuses == live statuses, forbids SUPPORTED on cgroup/uid_mappings/resource, requires non-zero exits behind every DEGRADED/UNSUPPORTED, and binds the profile to report bytes by sha256.
- ci.yml is honest about scope (no runtime-suite faking); rustup pin `1.98.1` matches local toolchain (`rustc 1.98.1 (48a229cea 2026-09-01)`).
- Existing three host profiles untouched by T03 (diff is pure addition); legacy `tests/pack` `@skipUnless(git)` is the only skip marker in tree, disclosed in `plans/active/T00.md` as debt — matches structure-lint exclusion comment.

Overall: **PASS with required follow-ups** — no blockers; T03 carries the three major findings (#1–#3) that should be fixed before the HostReport generator is reused on other hosts, plus the minor hygiene items (#4–#8).
