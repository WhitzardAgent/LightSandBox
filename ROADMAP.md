# v5 实施路线

完整任务图以specs/task-catalog.json为准，45个工作包均未获得产品完成认证。

| 里程碑 | 可运行交付 | 必须验证 |
|---|---|---|
| M0 | 仓库/源锁/host probe/协议与测试runner | T00/T01/T03/T41；真实权限与能力 |
| M1 | 预构建OCI→create→exec→files→delete | 真实退出、文件结果、无残留；同步测固定成本 |
| M2 | supervisor/PTY/输出和恢复 | API/manager/supervisor/worker不同故障域 |
| M3 | BuildKit与Harbor单服务 | 原始任务、真实BaseEnvironment、原始Verifier |
| M4 | Compose与网络/卷 | per-service操作、健康/依赖、独立验证、动态出口 |
| M5 | 原版OpenSandbox SDK与训练租约 | 无改SDK互操作；完整重建与旧身份拒绝 |
| M6 | 全量任务/故障/安全/密度/发行 | 完整分母、资源约束、公平基线、独立证据 |

T41冻结native合同，T42完成CLI/SDK核心链路生产化，T43实现按sandbox supervisor，T44检查协议漂移与声明。T38默认从已准备模板销毁/重建，不再依赖文件snapshot。

不要把seed参考测试计为T36 Harbor真实运行；不要把HTTPX mock计为T42生产SDK验收。计划可并行，但公共合同和锁文件只有一个集成所有者。
