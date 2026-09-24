# Secret handling rules (LSR series)

Scope: all LightSandbox components (runtime, supervisor, CLI, SDK, adapters, exam/verifier glue),
all host tiers. Normative language: MUST/MUST NOT/SHOULD.

## LSR-1 — Secrets MUST NOT enter argv (or inline interpreter source) of sandbox payloads

**Status: binding. Established 2026-09-24 by ADR 0001 Amendment A1 + P2.1 evidence.**

`/proc/<pid>/cmdline` is CONTENT_VISIBLE across co-located sandboxes on the current
PORTABLE_ROOTLESS ProcPolicy (`METADATA_AND_ARGV_VISIBLE`, evidence:
`evidence/security/proc-confidentiality.json`). Anything placed in argv — including
`python3 -c "<source>"` text — is world-visible to every other sandbox on the worker.

Therefore, these MUST NOT appear in argv or inline source of any sandbox process:

- API keys, access tokens, session credentials
- hidden verifier/evaluator tokens and expected answers
- candidate-identifying secret markers
- private service URLs carrying embedded credentials

**Preferred channels** (priority order):

1. Credential proxy on the supervisor side (sandbox talks to a localhost-scoped or
   fd-scoped proxy that never exposes the raw secret inside the sandbox)
2. Passed Unix file descriptor (fd inheritance, no path, no argv)
3. Protected file: inside-sandbox file owned by sandbox uid with mode 0400, written
   before payload start, removed at teardown (still readable by that sandbox's own
   processes by design — acceptable within one trust domain)

Environment variables: on the currently measured host, cross-sandbox `environ` reads
are errno 13 (P2.1). This is an OBSERVATION, not a guarantee: the contract does not
promise environ confidentiality on all ProcPolicy levels. Do not rely on env as a
secret channel unless the active HostCapabilities explicitly contract it
(`proc_confidentiality >= CONTENT_CONFIDENTIAL`). Until then treat env as
"convenient but not secret".

## LSR-2 — Secrets MUST NOT appear in logs, evidence, or test reports

`dev/check` outputs, spike evidence JSON, runc `--log` files, and error strings must
redact secret-shaped material. Test markers are random non-secret tokens by
construction (see P2.1 method); never reuse a real credential as a test marker.

## LSR-3 — No ambient secret import

The runtime MUST NOT inject host environment, host credential files, or agent-session
material into sandboxes implicitly. Every secret flow is explicit, auditable, and
scoped (see docs/v5/00_BRIEF non-goals: no ambient credential access).

## Relationship to trust model

PORTABLE_ROOTLESS v1 workers are single-trust-domain: co-located sandboxes belong to
one security tenant. LSR-1..LSR-3 additionally limit damage from misconfigured
callers and from any future ProcPolicy downgrade; they do not substitute for the
multi-tenant isolation that only a stricter ProcPolicy (private procfs) can give.
