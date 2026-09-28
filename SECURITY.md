# Security Policy

LightSandbox is a sandboxing project: isolation **is** the product, so we take
security reports seriously even before the first release.

## Supported versions

LightSandbox is **pre-release software**. There are no versioned releases yet,
and therefore no backport branches:

| Channel      | Security fixes |
| ------------ | -------------- |
| `main`       | Yes            |
| Any 0.x tag  | Not yet issued |

Security fixes land on the main branch only, until versioned releases exist.
Because the runtime is not yet complete, no deployed system is expected to
depend on this code today; if you are running anything built from this
repository, track main and treat all isolation guarantees as unproven (see
[README.md](README.md) and [docs/open-source-readiness.md](docs/open-source-readiness.md)).

## Reporting a vulnerability

Please report vulnerabilities **privately**. Do not open a public issue with
details of an exploitable flaw.

Preferred channel, in order:

1. **GitHub Private Vulnerability Reporting** — if enabled for this
   repository, use the *Security → Report a vulnerability* button on the
   repository page. (Repository owners: enable this under
   *Settings → Security → Private vulnerability reporting*; see
   [docs/development/repository-security.md](docs/development/repository-security.md).)
2. **Draft security advisory** — if private vulnerability reporting is not
   enabled, open a *draft* security advisory from the repository's
   *Security → Advisories* page. Drafts are private until published.
3. **Fallback** — if neither option is available to you (for example when you
   are looking at a mirror), open a minimal public issue that only asks for a
   private contact channel. Do not include technical details, reproducers, or
   affected code paths.

There is intentionally no email address in this policy; use the GitHub
channels above so that reports stay attached to the project.

## What to include

- Affected component and, ideally, commit.
- Reproduction steps or a proof of concept.
- Impact assessment (what isolation boundary or contract is breached).
- Whether you believe the issue is already public anywhere.

## Response process

These are our **current targets** during the pre-release period; they may be
revised as the project matures:

- **Acknowledgement** of your report within **7 days**.
- **Triage**: we confirm the issue, assess severity and scope, and keep you
  informed of status.
- **Fix and disclosure**: we aim to develop a fix and coordinate disclosure
  within **90 days** of triage. For isolation-critical flaws we will prefer
  faster disclosure; timelines may shift for issues requiring upstream
  coordination (runc, kernel, BuildKit).

## Scope

In scope:

- **Runtime isolation flaws** — anything that weakens guest↔host or
  guest↔guest isolation (namespace escape, shared mutable state, unsafe
  privilege transitions).
- **Contract and authentication bypass** — circumventing the native v1 API
  contract, admission checks, or capability-tier enforcement.
- **Supply chain of locked sources** — tampering with or substitution of the
  sha256-pinned upstream sources and artifacts recorded under `specs/`.

Out of scope:

- Benchmark/verifier logic issues that do not affect isolation (report these
  as normal bugs).
- Missing features or unimplemented backends.
- Vulnerabilities in third-party components with no LightSandbox-specific
  impact — report them upstream; do tell us if a locked upstream source we
  pin is affected.

## Safe harbor

We will not initiate legal action against researchers who act in good faith,
respect this policy, avoid accessing or destroying other users' data, and
give us a reasonable window to remediate before disclosure.
