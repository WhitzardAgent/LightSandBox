# Changelog

All notable changes to this project are recorded here.

Versioning scheme: this project uses **0.x development versions** until the
release gates are met — public API stability, native **and** kernel backends,
CLI/SDK, and documentation (see
[docs/open-source-readiness.md](docs/open-source-readiness.md); the gate
itself is `python3 tools/packctl.py release-check`). Until then there are no
versioned releases; `[Unreleased]` accumulates changes on the main branch.

## [Unreleased]

### Added

- Log streaming unit layer (bounded spool, SSE frame encoding; runtime
  layer pending worker).
- `lsguest serve` guest control agent: LSGP protocol v1 (framed, binary-safe,
  fail-closed), one active exec + async control plane, cancel/timeout,
  zero dependencies; unit-verified on unix (Linux/musl gate on CI/worker).
- Public documentation set: rewritten English README, getting-started,
  concepts (incl. security model, design trade-offs), tutorials,
  reference (CLI, Python SDK, errors, capability matrix, compatibility),
  and `docs/development/` (setup, testing, codebase map, backend authoring,
  repository security) — published via MkDocs Material, built `--strict`.
- Runnable examples: `examples/hello|python|coding-workspace|custom-image|
  capability-routing`, each covered by `./dev/check examples`.
- Verification suites: `./dev/check docs` (mkdocs strict build, markdown
  link and bash-snippet guards, generated-docs drift checks) and
  `./dev/check examples`; CI jobs for both.
- Contributor and security policies: `CONTRIBUTING.md`, `SECURITY.md`.
- Open-source readiness tracker: `docs/open-source-readiness.md`.
- GitHub issue templates (bug, feature request, compatibility report) and a
  pull request checklist.

### Changed

- Resource-semantics retest executed against kernel candidate-2 (verdict
  `RESOURCE_SEMANTICS_MISMATCH` 15/4: guest memory hard limit proven;
  cpu/pids enforcement unproven; results in
  `spike/results/uml-resource-semantics-candidate2.json`).
- This changelog was restructured into Keep-a-Changelog-style sections; the
  prior design-pack notes are preserved below.

### Fixed

- None recorded yet.

### Security

- Security policy published with a private reporting channel, scope
  definition, and current response targets (`SECURITY.md`).

## Historical design-pack notes (pre-release)

The notes below predate the changelog structure and are kept verbatim for
audit. They describe design revisions, not shipped releases.

### v5 修订摘要

#### 2026-09-24 宿主能力分层修订

- 引入 `HostCapabilityTier`（PORTABLE_ROOTLESS/FULL_ROOTLESS/ROOTFUL)，首发宿主假设为无root/无sudo的单ID userns主机；rootful降为未来可选backend。
- 区分 Benchmark Task Parity 与 General Docker Semantic Parity;PORTABLE_ROOTLESS 只承诺前者,依赖多UID/硬cgroup/特权挂载的任务按能力依赖入台账。
- 逐能力状态(SUPPORTED/DEGRADED/UNSUPPORTED/NOT_TESTED)绑定证据命令与退出码;UID/GID降级必须显式走SingleIdCompatibilityPolicy,禁止静默flatten。

#### 首批修订

- 原生EnvironmentSpec移除verifier/reward/agent；Harbor保留原Trial和Verifier。
- CLI统一lsbx；exec提交与execution管理分组，SDK同步/异步共用请求合同。
- 新增native OpenAPI/JSON schema/状态机/surface和75项本轮native reference测试。
- 首发每sandbox轻量supervisor；撤回“无shim职责即可低开销”的隐含假设。
- 命令argv/shell、空值、数量单位、generation/revision、Operation/Execution退出语义统一。
- 支持晚到权威证据确认UNKNOWN；不进行自动副作用重放。
- plan/prepare/create分开，构建预算与运行预算分开，训练destroy/recreate不依赖snapshot。
- 新增T41–T44；旧规范变为迁移导航，历史文本不进入实施上下文。
- 新增可执行客户端和CLI参考，不提供假运行时或伪一键加载。
