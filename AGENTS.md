# LightSandbox — repository instructions

Build an independent Linux sandbox: KVM-free, no host kernel replacement, no Docker/containerd daemon on candidate execution path. runc and isolated BuildKit OCI worker are reused. Rust runtime; Python SDK/CLI; optional Harbor adapter.

## Read progressively

Start with START_HERE.md. Current specifications: docs/v5/; native contracts: contracts/v1/. Choose one dependency-ready work package via `python3 tools/packctl.py next`, then use `context TASK_ID`. Do not read every document on every turn. Historical `history/` text is not implementation guidance.

## Do not conflate states

This package has tested reference logic and clients, NOT a running sandbox. `seed/` uses older internal protocols; `client/python/` is native-v1 reference. No mock or dry-run proves isolation, SDK interoperability or benchmark parity. Keep full benchmark denominators and original verifiers.

## Architecture boundaries

No Trial/Agent/Verifier/Reward in runtime core. Native API, CLI and SDK share one contract; OpenSandbox and Harbor adapt into it. Per-sandbox lightweight supervisor first; no implicit replay after lost execution results. API connection close never destroys guest resources. Launch mode and security policy are independent.

## Work safely

No host mutation or privileged runtime testing without an authorized worker. No Docker fallback, kernel change, arbitrary host hooks/mounts, ambient secret import, modified test oracle or fake success. Test before implementation; keep the real failure, logs, exact commands and actual result. Changes to shared contracts, lockfiles, migrations and evaluation rules need designated ownership/review.

## Checks

`python3 tools/packctl.py validate`
`python3 dev/check_native.py`
`python3 dev/check_seeds.py`
`python3 -m unittest discover -s tests/pack -v`
`./dev/check docs` (mkdocs strict + link/snippet/generated-docs guards)
`./dev/check examples` (runnable example suites; zero-collected = failure)

External docs (README, `docs/getting-started|concepts|tutorials|reference|development`) are product interface: no internal task IDs, no commands that cannot actually run, statuses synced with real evidence.

Current tests are reference/pack checks. Product tests and release require independent protected CI and real runtime evidence. Handoff via launch/CONTINUE.md and plans/templates/.
