# Independent Review — T41 (contract freeze, f8d1fbe+1855d7c) & T04 (domain core, 818498b)

- Reviewer: fresh-context verifier agent (round-03, user instruction §2)
- Date: 2026-09-25 · Scope focus: contract drift, backend-specific leakage into
  core, requirement-vs-observation separation, multi-backend readiness
- Verbatim verifier final report:

## Checks performed

- **Tree fidelity**: `git diff HEAD f8d1fbe/818498b` empty for all reviewed paths — working tree == committed deliverables.
- **Gates**: `cargo test -p ls-core` → 90/90 pass (at review time; 100/100 after round-03 backend.rs); `python3.12 -m unittest discover -s tests/contracts` → 27/27 pass (16 upstream-lock + 11 freeze); `packctl.py validate` ok; `check_native.py` 75/75; `check_seeds.py` 109/109. `tests/pack` (34 tests) showed 3–6 flaky errors — **traced to the concurrently-running UML spike build** (`spike/uml/work/src/build-bison/conftest.*` vanishing mid-`copytree` in `test_packctl.py:20`); out of scope, not a T41/T04 defect, but a test-isolation weakness worth noting to the spike owner.
- **Read end-to-end**: all 11 `contracts/v1` files incl. fixtures, `contracts/mod.rs` (978 lines, full), `native_v1_ref.py`, both contract test files, all 11 domain files, `lib.rs`; programmatic diffs and `jsonschema` probes for the corners fixtures don't cover.

## Contract-lock strength (baseline honesty check)

- `exec.schema.json` ≡ `openapi.ExecutionSpec` and `environment.schema.json` ≡ `openapi.EnvironmentSpec` are **byte-semantically identical** (verified by normalized diff).
- True lock tests exist for: state machines (`lifecycle.rs:123-228`), error codes (`error.rs:84-146` reads `error.schema.json` enum), idempotency KAT vs Python hashlib, fixture triple-validation (schema + Rust + Python).
- **However, the lock between the JSON schemas and the serde mirror is fixture-mediated, not schema-conformance-tested**: nothing validates the serde structs against the schemas (no `schemars` round-trip, no dynamic host-report validation). Several corners therefore pass silently — items 5, 6, 7 below.

## Findings (severity-ranked)

**1. BLOCKER — drift (T04 contradicts the T41-frozen exec contract).**
`control.rs` `ExecutionStart::parse` rejects **any** empty argv element (`control.rs:169-171`), while the frozen `exec.schema.json:76-77` explicitly *preserves* empty-string arguments at their position — pinned as an ACCEPT fixture (`fixtures/execspec/cases.json` `argv-empty-string-preserved`: `["echo",""]`, locked identically in Rust `mod.rs:135-148`, Python `native_v1_ref.py:89-101`, and the schema). Only `argv[0]==""` is rejected on the frozen surface. Additionally, the `execution.start` op (`control.rs:35-44`) has no representation for the frozen execspec's `env`, `cwd`, `user`, `timeout_ms`, or the `shell` command kind, and control `env` keys are checked only for NUL (`control.rs:129-141`) while the frozen schemas ban `=` and empty keys (`exec.schema.json:44-46`). Impact: an ExecutionSpec that passes the triple-validated frozen boundary cannot be forwarded API→manager — either rejected (empty args) or silently stripped (env/cwd/user/timeout). T08's create→exec slice will hit this immediately. No fixture covers the divergence. Fix: make `execution.start` embed the schema-normalized ExecutionSpec verbatim (plus `execution_id`); align argv/env rules with the frozen schema (control must relax).

**2. MAJOR — drift (two conflicting error envelopes, one frozen).**
`contracts/v1/error.schema.json` (FROZEN): `code` closed 12-value enum, `request_id` **required**. `native.openapi.json:1510-1541` `Error`: `code` free string, `request_id` optional. A wire error per the openapi violates the frozen schema. Fix: generate or lock the openapi Error component from `error.schema.json`; add a freeze test validating all openapi component envelopes.

**3. MAJOR — drift (wire schema forbids values the frozen identity semantics emit).**
`native.openapi.json:1419-1421,1481-1483` `revision: minimum 1` vs `ids.rs:36` `Revision::INITIAL = 0` and the identity fixture `recreate_incarnation` expecting `"revision": 0`. A server implementing the frozen identity fails its own openapi validation. Fix: relax openapi minimum to 0 (or re-freeze identity to start at 1).

**4. MAJOR — contract gap (ADR 0001 §6 reject-before-create unreachable on HTTP surface).**
Frozen `resolution-rules.json:5-9` promises unsatisfiable requirements are "REJECTED BEFORE CREATE … http_status 412". But `SandboxCreate` has `image` as a bare string and **no** `requires`/`single_id_compatibility_policy`/`image.{ref,user}` fields — the whole `sandbox-requirements.schema.json` model is not expressible through the declared creation path. Fix: add the requirements block to `SandboxCreate` (additive optional field).

**5. MINOR — drift (serde mirror more lenient than schema on closed enums).**
Seven schema closed enums mirrored as plain `String` in `contracts/mod.rs`: `ownership_model`(3), `image_user_support`(2), `ProcPolicy.mode`(2), `ProcPolicy.confidentiality`(3), `ProcessCaps.seccomp`(2), `FilesystemCaps.rootfs_driver`(3), `RuntimeCaps.supervisor`(2). Fix: real Rust enums with serde rename.

**6. MINOR — drift (serde edges disagree with schema in untested corners, both directions).**
(a) Schema accepts optional top-level `evidence` object; `HostCapabilities` `deny_unknown_fields` rejects it. (b) Schema `resources.*: minimum: 1`; `ResourceObservation: Option<u64>` silently accepts 0. (c) `ImageRequirement.user` accepts `""`/NUL where schema has minLength 1 + pattern. Fix: field add / parse-time enforcement / both-engine pin fixtures.

**7. MINOR — Rust/Python engine divergence (untested edge).**
`check_semantic_fact`: Python treats any falsy `exit_record` (`{}`,`[]`,`0`) as missing; Rust treats only JSON null as missing. Fixtures use only `null` or full objects. Fix: converge on one rule (null-or-absent only) + pin a fixture case locally in both engines' test files.

**8. MINOR — test-honesty (resolver "data-driven from frozen table" is half-true).**
Only Python drives requirement→capability mapping from `resolution-rules.json`; Rust hard-codes the mapping (`mod.rs:518-537`) and reads only `accepted_value`/code from the table. Re-pointing a capability path (exactly what multi-backend reframe wants) changes Python while Rust silently checks the old path. Fix: make Rust consume `capability` paths from the table, or lock the mapping with a table-vs-Rust test.

**9. MINOR — backend leak (reporting side only).**
Execution/state/request types are clean. But the host-capability report contract is Native-shaped: `runtime.runc{version,sha256,seccomp}` required — a UML-only host cannot produce a valid v1 report; `tier` enum is rootless-vocabulary-only; ls-core carries TWO disagreeing tier enums (`contracts::Tier` with UnsupportedHost vs `capability::HostCapabilityTier` without) and two host-capability models (T03 findings-list vs T41 dimensioned). Fix: per-backend runtime facts (`runtime.{native:{runc:…}, uml:{…}}` or optional); converge tier/model in the round-03 backend-neutral pass.

**10. NOTE — cosmetic/terminology only.**
`execution.rs:369` literal `"runc create receipt"` test label (harmless). openapi `termination_reason` enum vs domain `TerminationReason` — no stated mapping; flagging for the unification pass.

## Explicit non-findings

- Execution/state leakage — none. `SandboxState`/`ExecutionState`/`OperationKind`/`TerminationReason`/`ExecSpec`/`Capability` vocabulary is backend-neutral and UML-satisfiable; state machines carry no OCI-runtime lifecycle binding. `BundleRef` digest-addressed, `GuestPath` guest-internal — both fine for UML.
- ADR §6 enforcement — works as specified; 27/27 green.
- Domain core correctness — no defects in idempotency/fencing/resources/migrations.

## Verdict

1. The contract/domain **can carry a second backend at the execution/guest-contract layer without breaking changes**; gaps are in the host *report* schema and internal contract coherence.
2. MUST move before UML work: reconcile `control.rs::execution.start` with the frozen ExecSpec (#1); per-backend/optional `runtime.runc` + converge tier models (#9); unify Error envelope + `SandboxInfo.revision` + `SandboxCreate.requires` (#2–4); table-driven Rust resolver (#8).
3. Fine as-is: the entire T04 domain core, state-machine and error-code locks, exec/environment schema–openapi equality, §6 rejection semantics.

**Overall: PARTIAL-PASS leaning FAIL on contract coherence** — individually well-tested and honest, but as a combined freeze they ship mutually contradictory surfaces (#1 hard contradiction; #2–4 two authorities).

---

## Round-03 disposition (recorded by implementer)

| # | Severity | Disposition |
|---|---|---|
| 1 | BLOCKER | FIX THIS ROUND (T08-blocker): control.rs embeds schema-normalized ExecutionSpec verbatim; argv/env rules aligned to frozen schema; control fixture extended |
| 2 | MAJOR | FIX THIS ROUND: openapi Error component aligned to frozen error.schema.json (required request_id + 12-enum) + openapi envelope validation test |
| 3 | MAJOR | FIX THIS ROUND: openapi revision minimum 1→0 (identity truth `Revision::INITIAL=0` wins) |
| 4 | MAJOR | FIX THIS ROUND: additive optional `requires` + `single_id_compatibility_policy` block on SandboxCreate |
| 5 | MINOR | FIX THIS ROUND: seven closed String fields → serde enums (wire-compatible for valid values; invalid now rejects) |
| 6a | MINOR | FIX: `evidence: Option<Value>` added to serde mirror |
| 6b/6c | MINOR | Deferred to T45 contract-evolution pass; pinned as divergent-corner fixtures then |
| 7 | MINOR | FIX: Python converges to Rust rule (null-or-absent only) + local fixture cases both engines |
| 8 | MINOR | FIX: Rust resolver consumes capability paths from the frozen table (true data-driven parity) |
| 9 | MINOR | Deferred to T45 (per-backend runtime facts, tier/model convergence); ADR 0002 §9/§14 records |
| 10 | NOTE | FIX: test label renamed "backend create receipt" |
