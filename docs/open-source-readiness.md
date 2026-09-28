# Open-Source Readiness

Status of the items that must be settled before LightSandbox is presented as
an open-source project. Last reviewed: 2026-09-28.

## Owner decisions required

These items **cannot** be completed by contributors because they require
choices only the project owner can make. Do not create the files below on
your own initiative.

### LICENSE — decision pending

- The Cargo workspace already declares `license = "Apache-2.0"` in
  `Cargo.toml` (inherited by all crates via
  `license.workspace = true`), but **no LICENSE file exists** in the
  repository root.
- The owner must (1) confirm Apache-2.0 is the intended license for the whole
  repository — including the Python parts, contracts, and documentation —
  and (2) drop the license text into `LICENSE` at the root.
- Until then, treat the code as "license declared in package metadata only".

### CODE_OF_CONDUCT.md — decision pending

- No code of conduct exists.
- Recommendation: adopt the
  [Contributor Covenant](https://www.contributor-covenant.org/) (current
  version), which is the de-facto default for open-source projects.
- The owner must choose the enforcement contact recorded inside it before it
  lands; that contact is a governance decision, not a documentation one.

### CITATION.cff — decision pending

- No citation file exists. `CITATION.cff` lets GitHub render a "Cite this
  repository" widget.
- Authorship and affiliation fields must not be guessed by contributors. To
  land it, the owner fills in:

| Field            | Needed value                          |
| ---------------- | ------------------------------------- |
| `authors`        | Legal names / preferred handles       |
| `affiliation`    | Per author, optional                  |
| `orcid`          | Per author, optional                  |
| `title`          | Project title for citation            |
| `version`        | First citable version (post-gates)     |
| `date-released`  | Date of that version                  |
| `url` / `repository-code` | Canonical repository URL       |
| `license`        | Must match the LICENSE decision above |
| `keywords`       | e.g. sandbox, isolation, OCI, runc    |

## Repository hygiene checklist

| Item                                   | State    | Notes |
| -------------------------------------- | -------- | ----- |
| README                                 | Done     | `README.md` |
| CONTRIBUTING                           | Done     | `CONTRIBUTING.md` |
| SECURITY                               | Done     | `SECURITY.md` |
| CHANGELOG                              | Done     | `CHANGELOG.md`, Keep-a-Changelog sections + preserved history |
| CI gates                               | Done     | `.github/workflows/ci.yml`; unprivileged gates only by design |
| Issue templates / PR checklist         | Done     | `.github/ISSUE_TEMPLATE`, `.github/pull_request_template.md` |
| Developer documentation                | Done     | [development/setup.md](development/setup.md) and siblings |
| LICENSE file                           | Pending  | Owner decision, see above |
| CODE_OF_CONDUCT.md                     | Pending  | Owner decision, see above |
| CITATION.cff                           | Pending  | Owner decision, see above |
| GitHub security features               | Pending  | Admin-side settings, see [development/repository-security.md](development/repository-security.md) |
| Versioned release                      | Blocked  | Gated by `python3 tools/packctl.py release-check`, see below |

## Release gating

A versioned release is intentionally hard to cut:

- Scheme: 0.x development versions until the gates are met — stable public
  API, native **and** kernel backends, CLI/SDK, and complete documentation.
- The mechanical gate is `python3 tools/packctl.py release-check`, which fails
  while any product task is unverified, any lock is incomplete, host/benchmark
  status is unverified, or the independent release approval in
  `release/release-gate.json` is missing.
- The gate currently blocks by design: the repository contains tested
  reference logic, not a running sandbox (see `AGENTS.md`).
  Shipping a release requires real runtime evidence reviewed by an
  independent party — checklists in this file do not replace that.
