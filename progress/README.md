# LightSandbox 实现轮次记录(progress/)

任何一轮开发都不能只在聊天中汇报;每一轮必须有本目录下的长期记录文件。
文件命名:`YYYY-MM-DD-round-NN.md`。不要覆盖旧轮次文件;同一轮内允许持续更新本轮文件。

## 轮次总表

| Round | Date | Start Commit | End Commit | Milestone | Tasks | Status | Key Decision | Progress File |
|---|---|---|---|---|---|---|---|---|
| 01 | 2026-09-24 | bc69c5c | 39200e3 | 首发宿主定位 + T00/T01/T03 + Portable Rootless feasibility + ADR 0001 | T00, T01, T03, spike(9 实验), ADR 0001 | COMPLETED | v1 backend = native rootless runc + 显式降级(ADR 0001 选项 B) | [2026-09-24-round-01.md](2026-09-24-round-01.md) |
| 02 | 2026-09-24 | 39200e3 | (进行中) | T41 合同冻结 + T04 核心域 + 安全补强(/proc、seccomp、supervisor) | T00–T03 review, P2.1, seccomp spike, T41, T04, runtime spine | IN_PROGRESS | (进行中) | [2026-09-24-round-02.md](2026-09-24-round-02.md) |

## Status 词汇(只允许这四个)

- `IN_PROGRESS` — 本轮正在执行
- `PARTIAL` — 本轮收尾,但约定工作只完成一部分(必须在文件里列清未完成项)
- `COMPLETED` — 本轮**约定的工作**完成;**不表示**所有 task 已 independent verified
- `BLOCKED` — 被依赖/权限/安全问题阻塞,无法继续执行

## 任务状态与轮次状态的区别

轮次 status 描述"这一轮约定的工作";task 生命周期仍按
`planned → in_progress → implemented → in_review → verified`(docs/harness/04)。
`verified` 需要已验证依赖 + 可校验测试证据文件 + 审查记录,且属于受保护变更,
必须由不同责任人/不同上下文的审查完成,实施者不得自 mark。
