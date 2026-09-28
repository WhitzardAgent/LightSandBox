## Scope

<!-- What does this PR change, and what does it deliberately not change? -->

## Task card

<!-- Task id from plans/tasks/ (e.g. T12), or "none — standalone fix". -->

## Tests run

<!-- List every command actually executed, with raw outcomes: exit codes and
collected/passed/failed/blocked counts. Copy from the run summaries under
dev/.check-runs/ where applicable. Do not write "tests pass" without data. -->

| Command | Exit code | Tests (collected/passed/failed/blocked) |
| ------- | --------- | --------------------------------------- |
|         |           |                                         |

## Evidence

<!-- Raw logs/artifacts added under evidence/, or reason none is needed. -->

## Documentation

<!-- Docs updated, or "not applicable". Internal links in new Markdown are
validated by packctl — run `python3 tools/packctl.py validate` before pushing. -->

## Contracts and lockfiles

- [ ] `contracts/v1`, `specs/`, and lockfiles are untouched, **or** the change
      is owner-approved and named here.

## Honesty

- [ ] No result in this PR is faked, rounded up, or silently degraded
      (blocked suites are reported as blocked, failures as failures).
- [ ] Test criteria, oracles, verifiers, and security profiles were not
      modified to obtain a pass.
