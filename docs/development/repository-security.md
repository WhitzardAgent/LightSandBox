# Repository Security

Operational security posture for the LightSandbox repository: the GitHub
settings an **admin** should enable, supply-chain basics for Rust and Python,
and the design for release artifacts. Project-side vulnerability handling is
in `SECURITY.md`.

## GitHub settings (admin checklist)

Enable these under the repository's *Settings → Security* (and *Code
security and analysis*):

| Setting | Why |
| ------- | --- |
| **Dependabot alerts** | Notice when a locked dependency gains a known advisory. |
| **Dependabot security updates** | Get candidate fix PRs — but see "advisory handling" below before merging anything. |
| **Secret scanning** | Catch committed credentials in a repo that pins upstream sources and CI config. |
| **Push protection** | Block secrets at push time, not after. |
| **Code scanning / CodeQL** | Static analysis for the Python tooling; Rust support is limited, treat as additive. |
| **Private vulnerability reporting** | Gives reporters the private channel promised in `SECURITY.md`. |

Keep branch protection on `main` aligned with the project rule that verified
status requires trusted CI plus independent review — the harness repository
checks alone are not a release gate (see
`open-source-readiness.md`).

## Rust supply chain

- **`Cargo.lock` is committed** — it is the lock; treat diffs to it as
  review-relevant changes, not noise.
- **License visibility**: `cargo metadata` exposes each dependency's declared
  license; the workspace itself declares Apache-2.0 (see
  `Cargo.toml`). Run `cargo metadata` (and, when adopted, a
  license summarizer) before releases to audit the dependency set.
- **Later hardening gates (planned, not installed)**: `cargo-deny` (license
  policy, duplicate/ban detection) and `cargo-audit` (RustSec advisories) are
  candidates for a CI gate. Their installation is deferred until the runtime
  dependency tree settles; do not add them ad hoc to developer machines as
  required tooling.
- **Advisory handling rule**: advisories are *recorded* (in the risk register
  or a task card) with an explicit decision per advisory — bump, pin, accept
  with justification, or not affected. Never mass-upgrade the lockfile
  silently to make an advisory scanner go green.

## Python supply chain

- Follow the existing pinning pattern:
  `dev/requirements-reference.txt`
  records exact versions present in the authoring environment; CI installs
  those exact versions. Keep adding exact pins this way.
- **Do not introduce a new dependency-management system** (pip-tools, Poetry,
  uv, hatch, …) without owner approval — one consistent, reviewable mechanism
  is the principle.
- New dependencies enter through a task card with rationale; check license
  and maintenance state first.

## Release artifact design (future)

Planned artifacts once the release gates pass (`python3 tools/packctl.py
release-check`; currently blocking by design):

| Artifact | Notes |
| -------- | ----- |
| `lightsandboxd` (host daemon) | Rust binary from the workspace |
| `ls-guest` | static musl build; runs inside the guest, must not drag host libc assumptions |
| Python SDK | versioned package from `client/python` |
| CLI (`lsbx`) | versioned alongside the SDK, sharing the request contract |
| UML kernel artifact | kernel backend runtime asset, hash-locked like other upstream artifacts |

Every published artifact ships with:

- its **version**, the exact source **commit**, and a **sha256** digest;
- provenance tying it to the CI job that built it;
- an **SBOM** (required at release per current OpenSSF guidance — SPDX or
  CycloneDX, generated from the committed lockfiles, not from memory).

Artifacts without reproducible digests or SBOMs are not releasable; this
list is a design commitment, not a claim that any of these artifacts exist
today.
