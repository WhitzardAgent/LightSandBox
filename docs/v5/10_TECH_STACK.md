# 技术栈：减少可变路径，保留成熟原语

## 选型结论

Rust运行时不改回Python；Python承担独立SDK/CLI参考与Harbor适配。Compose normalizer保留一个小Go辅助程序，因为完整Compose语义不值得在Rust或Python中重新猜测。没有microVM、多provider、分布式DB或调度平台。

| 能力 | 选择 | 约束与不选理由 |
|---|---|---|
| 异步服务 | Tokio 1.x | 阻塞SQLite/镜像工作外移，有界channel，不跨await持DB事务 |
| HTTP | Axum 0.8系 + Tower + 按需tower-http | 两个协议门面共享应用服务；不再多部署FastAPI |
| JSON/错误 | Serde、serde_json、thiserror | native/compat模型分开；无自由dict穿透到root服务 |
| Unix/FD | rustix 1.x、OwnedFd | 优先安全封装、默认CLOEXEC；局部unsafe需要审查及测试。[S08] |
| OCI | runc二进制 + oci-spec | 第一次只有runc后端；不嵌Docker SDK、不重写隔离机制 |
| 持久化 | rusqlite/SQLite WAL | manager单写者；大输出存文件；同步等级和fsync点明示 |
| 日志 | tracing/tracing-subscriber | 输出日志与控制日志隔离；禁止命令secret默认入日志 |
| 宿主HTTP | reqwest/rustls | TLS校验、禁止不受控跨源凭据转发 |
| 原生CLI首发实现 | Python argparse＋同包HTTPX SDK | CLI不常驻沙盒，复用现有客户端；不平行开发Rust CLI。仅在独立分发需求明确后再评估clap |
| Python SDK | HTTPX同步/异步 | 本轮实际测试0.28.1；正式传递依赖锁仍需生成。[S09] |
| Python测试 | unittest标准库；生产pytest/pytest-asyncio可选 | 本包现有回归不强换框架；不要为了统一格式重写绿色测试 |
| Python类型 | dataclasses/严格函数；生产可选Pydantic | 字段来源只有contracts/v1，不复制枚举 |
| Compose | compose-go/v2 CLI | 只规范化IR；实际服务、卷和依赖行为由Environment engine做。[S15] |
| 构建 | 固定BuildKit OCI worker | buildctl→buildkitd；可独立构建机，无containerd执行依赖。[S04] |
| 镜像 | skopeo/umoci（初期helper） | 明确传参与hash；helper特权和文件输出受限制 |
| 网络 | CNI bridge/host-local/loopback＋服务DNS＋策略管理器 | CNI并不等同于完整网络/防火墙；先锁一个配置再支持变体 |
| 内容哈希/ID | sha2、uuid | ID与digest分开；owner权限不由digest推导 |
| 测试/供应链 | proptest、tempfile、cargo audit/deny、SBOM | 工具成功不等于无风险；独立review覆盖unsafe、shell、FD、路径与网络 |

## 二进制交付

生产交付包括operator安装的Rust服务/相关helpers及用户端SDK；`pip install`不会授予权限或安装内核能力。SDK包可独立安装到评测/训练进程。Harbor依赖在独立extra/包，基础SDK不拉全模型生态。

四个Rust crate生成api/manager/supervise/guest等必要binary，crate数量与进程数量不是一回事。进程隔离按故障与权限边界决定，不能为了“单binary”合并为一个root联网进程。

编译器版本、目标CPU、glibc/musl、宿主内核、权限五项独立记录。生产必须锁Cargo.lock、Rust toolchain、Go module sum、Python lock、runc/BuildKit/CNI等二进制摘要和许可证。本包保留未锁状态；选型版本族不等于完整可复现构建。

## 首发不做

第二种低层runtime、rootless自动降级、所有cgroupv1/v2路径、跨主机迁移、RAM checkpoint、GPU/Windows广泛支持、自动裸金属扩容、独立训练调度器。完整任务清单若需要其中任一能力，记录阻塞并安排独立实现，不从总任务数删除。

## 工程成本优先级

1. 安全可恢复的生命周期＋真实退出/PTY/文件语义。
2. Harbor与OpenSandbox接口/镜像/网络行为一致性。
3. 安装、诊断、锁定、升级回滚。
4. 吞吐与常驻开销优化。

第一次密度对比即使没有领先Docker也要保留数据，找到真正固定成本后再优化。Docker自身也使用共享只读层/page cache，不能把基础机制当新性能结论。[S05]
