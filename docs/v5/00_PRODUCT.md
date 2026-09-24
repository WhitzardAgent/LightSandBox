# LightSandbox v5 产品定义与权威入口

日期：2026-09-24。交付：经过审查的产品设计、原生客户端/CLI参考实现、可执行测试、开发工作包。**尚无可投入生产的沙盒服务。**

## 固定的产品目标

一个可独立安装的 Linux 沙盒服务。**首个部署目标是无 root、无 sudo、无 KVM 的普通 Linux VM 上的普通用户（Portable Rootless Host)**：不假定 subuid/subgid、cgroup 委托、/dev/fuse 或可写私有 proc。rootful 部署仅为未来可选 backend，不是当前主要目标。无需 KVM、嵌套虚拟化或替换内核。候选执行链路不调用 Docker/containerd daemon，镜像构建允许独立 BuildKit OCI worker。Linux 能力、安全补丁、权限、文件系统、CPU 架构和用户态 ABI 仍须分别满足要求。[S01/S04]

外部提供三条入口：原生 CLI/Python SDK；固定版本 OpenSandbox 协议门面；Harbor 第三方 Environment provider。三个入口共用领域服务，禁止各写一套生命周期实现。Harbor 的任务组织、模型、验证和奖励仍由 Harbor/调用方负责。

固定版本 CyberGym、SWE-bench、Terminal-Bench 的**完整任务集合**保持最终验收目标。环境可运行、结果一致、正向 oracle 可用是三个独立指标。缺镜像、参考故障、没有有效 PoC、宿主不支持等全部保留在总台账。进入不了某个任务的执行路径，就不能宣布该任务已经兼容。[全量合同](../acceptance/01_PARITY_CONTRACT.md)

本项目追求的是 **Benchmark Task Parity**（固定任务集合上的环境/操作/验证一致），不是 **General Docker Semantic Parity**（任意 Docker 负载语义全等）。宿主按 HostCapabilityTier 分层（见[领域合同 §宿主能力分层](03_DOMAIN.md)）;PORTABLE_ROOTLESS 上依赖多个真实容器 UID、硬 cgroup 限额、特定内核特性、特权挂载或宿主网络管理的任务，必须按能力依赖如实入台账，不能通过 flatten 用户、删改 verifier 或修改任务计入兼容。

## 明确的三项取舍

1. 复用 runc 实现 OCI 隔离，集中开发其上层环境服务。不会自写 syscall 拦截器、内核或 Dockerfile 解释器。
2. 可靠性优先的首发监督器以每个沙盒一个小进程隔离故障，运行时不驻留 Python/HTTP 业务服务。共享监督器是后续有实测数据才允许的优化。
3. 高密度是可检验的目标，不预设比 Docker 快。完整任务支持不等于完整 Docker Engine API、任意特权负载或所有未来 Harbor 版本支持。

## 读者导航

| 读者 | 阅读顺序 |
|---|---|
| 项目负责人 | 本文 → [审查结论](01_AUDIT.md) → [架构](02_ARCHITECTURE.md) → [开发顺序](11_DELIVERY.md) |
| CLI/SDK 开发者 | [领域合同](03_DOMAIN.md) → [CLI](04_CLI.md) → [SDK](05_SDK.md) → [HTTP](06_HTTP.md) |
| Runtime 开发者 | [架构](02_ARCHITECTURE.md) → [故障协议](08_FAILURES.md) → [执行环境](09_RUNTIME_DETAILS.md) |
| Harbor 开发者 | [Harbor](07_HARBOR.md) → 锁定上游 → 基于原版接口运行契约与真实环境测试 |
| 首次 Coding Agent | 根目录 AGENTS.md → START_HERE.md → `packctl context <task>` |

## 权威顺序与状态

v5 文档、`contracts/v1/` 是新合同来源；`specs/task-catalog.json` 是开发任务来源；测试报告是已验证状态来源。旧路径被改为迁移入口，不并存两套规范。旧 seed 只作为参考实现和回归资产，不能直接认定它们满足原生 v1 协议。

代码状态分为 `reference-tested`、`planned`、`runtime-verified`、`parity-verified`。本包没有任何组件被标为后两类。已实现范围见 [实现与验证状态](14_STATUS.md)。来源统一见 [来源](15_SOURCES.md)。
