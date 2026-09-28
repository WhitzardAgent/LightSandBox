# Adding a Backend

LightSandbox supports multiple execution *backends* behind one contract. The
two reference points are the **native** backend (rootless OCI/runc on the
host, `adr/0001-portable-rootless-runtime.md`) and the
**kernel** backend (UML-based, under feasibility research in
`spike/uml` per
`adr/0002-capability-adaptive-backends.md`). This page is the
checklist a third backend must satisfy. It describes the contract; the
internal design authority remains `v5`.

## Non-negotiable rules

1. **Backend-neutral supervisor interface.** The `InstanceSupervisor`
   abstraction must not expose backend internals: no runc state root, no
   bundle path, no pid-namespace FD handles leak across the boundary. A
   backend adapter translates between the neutral interface and its own
   machinery — it never widens the interface for its convenience.
2. **No benchmark vocabulary in the runtime core.** Terms like
   `Trial`/`Verifier`/`Reward`/`Evaluation` are lint-forbidden in the crates
   (`./dev/check structure` enforces this). Backends serve the native v1
   contract, not any benchmark harness.
3. **Fail-closed contracts.** If a capability, image, or state cannot be
   established truthfully, the answer is an error — never a downgraded guess.
   Unknown stays `UNKNOWN` until late-arriving authoritative evidence
   resolves it.
4. **No silent capability switches.** Degradations (e.g. UID/GID flattening)
   go through the explicit compatibility policy with a recorded decision.
   A backend must not quietly offer less than it claimed at admission.
5. **Launch mode and security policy stay independent**; a backend does not
   conflate them.

## Backend contract checklist

### 1. Capability offer

The backend declares what the host can actually deliver at the
`HostCapabilityTier` level (`PORTABLE_ROOTLESS` / `FULL_ROOTLESS` /
`ROOTFUL`, see `contracts/v1/host-capabilities.schema.json`) plus per-axis
capability states: `SUPPORTED` / `DEGRADED` / `UNSUPPORTED` / `NOT_TESTED`.
Every claim binds to evidence (command + exit code), not intention. The
host probe tooling lives in `python/src/lightsandbox_dev`.

### 2. Planner integration

Planning (`plan`) must work offline and truthfully: given an
`EnvironmentSpec`, produce the backend-specific plan with image resolution,
rootfs strategy, and budget separation (build budget vs run budget).
`plan`/`prepare`/`create` are distinct steps; a backend does not collapse
them. Where the plan cannot be honored, planning fails closed.

### 3. Materializer

Owns turning the plan into concrete resources: image fetch/unpack metadata,
rootfs construction strategy, namespace/mount topology for native, kernel +
rootfs wiring for kernel-style backends. All inputs from locked sources are
hash-verified; nothing is fetched ad hoc at materialize time.

### 4. Supervisor

Each sandbox gets a per-instance lightweight supervisor process implementing
the backend-neutral `InstanceSupervisor` interface: lifecycle
(start/stop/destroy), state reporting, exit observation, and loss detection.
The supervisor owns the sandbox's lifetime; see the domain docs for why the
"no shim responsibility" assumption was retracted. Closing an API connection
must never destroy guest resources — the supervisor survives clients.

### 5. Exec semantics

Command execution follows the native v1 semantics exactly: argv/shell
distinction, empty-value handling, quantity units, generation/revision
tracking, and Operation/Execution exit semantics as frozen in
`contracts/v1`. There is no automatic side-effect replay after a lost
execution result; a lost result is reported, not guessed.

### 6. Cleanup

Destroy/recreate must not depend on snapshots by default. All live resources
(instances, runs, leases) are enumerated in the handoff record with their
ids; cleanup is observable and its failures are reported, not swallowed.

### 7. Reconcile

After a supervisor or daemon restart, the backend reconciles desired vs
observed state truthfully: what still exists, what was lost, what is
`UNKNOWN` pending authoritative evidence. Reconcile never fabricates state
to look healthy.

### 8. Tests

- Unit tests in the crates, run by `./dev/check unit` — always required.
- Contract tests against `contracts/v1` fixtures, proving the backend honors
  the frozen surface (`./dev/check contracts --task <T>`).
- Worker-gated runtime/differential/security coverage once an authorized
  worker exists (`./dev/check runtime --task <T> --worker <W>`). Until then
  the honest state is exit `3` blocked — say so.

### 9. Evidence

A backend is a claim about isolation; claims need raw evidence under
`evidence/`: exact commands, exit codes, test counts, host report, and
sha256-hashed logs per
`specs/evidence-contract.json`. The
isolation axes exercised (the spike used a 13-axis matrix as its reference
point) belong in the evidence, not in prose. Verified status is granted only
by the integrator after trusted CI and independent review.

## Worked example: native vs kernel

| Contract item | Native backend | Kernel backend |
| ------------- | -------------- | -------------- |
| Capability offer | Rootless userns tiers; single-ID host starts as `PORTABLE_ROOTLESS` | Needs kernel artifact + host support; researched in `spike/uml` |
| Materializer | umoci/runc rootless unpack, rootfs strategies (full copy; tmpfs-COW needs supervisor blocks) | UML kernel + rootfs image assembly |
| Supervisor | Per-sandbox supervisor around runc lifecycle | Per-sandbox supervisor around the UML process |
| Exec semantics | exec into the OCI container namespaces | exec via the guest protocol served by `ls-guest` |
| Cleanup | OCI state + rootfs teardown | UML process + image teardown |
| Isolation axes | userns/mount/pid/net from the host | adds a guest kernel boundary |

What must **not** differ between them: the API contract, the supervisor
interface shape, exec semantics, and the honesty rules. Capability registers
differ; contracts never do.
