# reviews/ — 审查记录档案

按 docs/harness/03(独立审查三视角)与 docs/harness/04(verified 需要审查记录)归档。
审查者身份在每份报告头部明示。harness 支持的 fresh-context verifier 子代理 =
"同一模型的新会话提供的不同上下文审查"(harness/03:15),**不等同于独立机构安全评估**;
实施者与审查者在本仓库必须是不同会话/不同上下文。

状态词汇:每份报告给出 per-commit verdict(CLEAN / CLEAN_WITH_NOTES / MAJOR / BLOCKER)
与 findings(blocker/major/minor)。涉及权限提升、失控网络、结果伪造、资源残留或判据
变更的问题阻断合并;修复由实施分支完成并重跑检查(harness/03:21)。

## 索引

| 日期 | 报告 | 范围 | 结果 |
|---|---|---|---|
| 2026-09-24 | [2026-09-24-round-01-tasks-review.md](2026-09-24-round-01-tasks-review.md) | `5b95372` design, `b92736c` T00, `957fde7` T01, `95fadff` T03 | PASS with required follow-ups(0 blocker / 3 major 全在 T03 / 7 minor+info) |
| 2026-09-24 | [2026-09-24-round-01-spike-adr-review.md](2026-09-24-round-01-spike-adr-review.md) | `2138549` spike 证据 + ADR 0001 | (待落盘) |

## 修复追踪

| finding | 修复 commit | 状态 |
|---|---|---|
| tasks-review #1 derived_tier 静默改写 UNSUPPORTED_HOST→PORTABLE_ROOTLESS | (修复中) | open |
| tasks-review #2 probe_network 在调用者 netns 建 veth | (修复中) | open |
| tasks-review #3 ls-core derive_tier 与 T03 报告 finding 命名不通 | (修复中) | open |
| tasks-review #5 lock 工具缺负控测试 | (修复中) | open |
| tasks-review #6 nested_container_traits 硬编码 | (修复中) | open |
| tasks-review #9 dev/checklib flag 吞 token | (修复中) | open |
| tasks-review #4/#7/#8(卡范围漂移/红日志 0 字节/CI 部署级 gap) | 记录于 progress round-02(卡修改属受保护变更,走集成 pass) | recorded |
